use std::collections::HashMap;
use std::process::Stdio;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::board::{self, Board};
use crate::config::{Config, HookAction};
use crate::hook::{self, CommentTracker, HookOutput};
use crate::payload::Payload;

/// `(lowercased repository, issue number)`
pub type IssueKey = (String, u64);

pub fn issue_key(repository: &str, number: u64) -> IssueKey {
    (repository.to_ascii_lowercase(), number)
}

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Terminal event other than key (e.g. resize)
    Redraw,
    /// WebSocket connection status
    Status(String),
    /// Payload received from WebSocket
    Received(Payload),
    /// Output of a running hook action (log tag, output)
    HookOutput(String, HookOutput),
    /// Hook action finished (issue, log tag, exit code)
    HookDone(Option<IssueKey>, String, Option<i32>),
    /// Hook actions were not run (issue, count of actions, reason)
    HookSkipped(Option<IssueKey>, usize, String),
    Board(Result<Board, String>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogKind {
    Event,
    Skipped,
    Command,
    Stdout,
    Stderr,
    Exit,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub kind: LogKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    Board,
    Log,
}

pub struct App {
    config: Config,
    tx: UnboundedSender<AppEvent>,
    /// Hook worker per issue (`None` for events without issue number)
    hook_workers: HashMap<Option<IssueKey>, UnboundedSender<HookJob>>,

    pub board: Option<Board>,
    pub board_error: Option<String>,
    pub loading: bool,
    /// Refresh requested while loading
    refresh_pending: bool,
    /// Issue -> count of hook actions not yet finished
    pub active_issues: HashMap<IssueKey, usize>,
    pub log: Vec<LogLine>,
    pub status: String,

    pub focus: Focus,
    pub board_scroll: u16,
    /// `None` follows the latest log
    pub log_scroll: Option<u16>,
    /// Scroll position of the bottom of the log, updated by the renderer
    pub log_bottom_hint: u16,
    /// Pane areas and clickable issue rows, updated by the renderer
    pub board_area: Rect,
    pub log_area: Rect,
    pub issue_links: Vec<(Rect, IssueKey)>,
    pub quit: bool,
}

type HookJob = (Payload, Vec<HookAction>);

impl App {
    pub fn new(config: Config, tx: UnboundedSender<AppEvent>) -> Self {
        Self {
            config,
            tx,
            hook_workers: HashMap::new(),
            board: None,
            board_error: None,
            loading: false,
            refresh_pending: false,
            active_issues: HashMap::new(),
            log: vec![],
            status: String::new(),
            focus: Focus::Board,
            board_scroll: 0,
            log_scroll: None,
            log_bottom_hint: 0,
            board_area: Rect::default(),
            log_area: Rect::default(),
            issue_links: vec![],
            quit: false,
        }
    }

    pub fn refresh_board(&mut self) {
        if self.loading {
            self.refresh_pending = true;
            return;
        }
        self.loading = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = board::fetch().await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Board(result));
        });
    }

    pub fn handle(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.on_key(key),
            AppEvent::Mouse(mouse) => self.on_mouse(mouse),
            AppEvent::Redraw => {}
            AppEvent::Status(status) => self.status = status,
            AppEvent::Received(payload) => self.on_received(payload),
            AppEvent::HookOutput(tag, output) => self.on_hook_output(&tag, output),
            AppEvent::HookDone(issue, tag, status) => {
                self.finish_actions(issue.as_ref(), 1);
                let status = status.map_or("killed".to_string(), |c| c.to_string());
                self.push(LogKind::Exit, format!("  [{tag}] exit {status}"));
            }
            AppEvent::HookSkipped(issue, count, reason) => {
                self.finish_actions(issue.as_ref(), count);
                self.push(LogKind::Skipped, format!("  skipped: {reason}"));
            }
            AppEvent::Board(result) => {
                self.loading = false;
                match result {
                    Ok(board) => {
                        self.board = Some(board);
                        self.board_error = None;
                    }
                    Err(e) => self.board_error = Some(e),
                }
                if std::mem::take(&mut self.refresh_pending) {
                    self.refresh_board();
                }
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('r') => self.refresh_board(),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Board => Focus::Log,
                    Focus::Log => Focus::Board,
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            KeyCode::Char('G') | KeyCode::End if self.focus == Focus::Log => self.log_scroll = None,
            _ => {}
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        let pos = Position::new(mouse.column, mouse.row);
        let pane = if self.board_area.contains(pos) {
            Focus::Board
        } else if self.log_area.contains(pos) {
            Focus::Log
        } else {
            return;
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = pane;
                let link = self.issue_links.iter().find(|(r, _)| r.contains(pos));
                if let Some((_, (repository, number))) = link.cloned() {
                    self.open_issue(repository, number);
                }
            }
            MouseEventKind::ScrollDown => self.scroll_pane(pane, 1),
            MouseEventKind::ScrollUp => self.scroll_pane(pane, -1),
            _ => {}
        }
    }

    fn open_issue(&mut self, repository: String, number: u64) {
        self.status = format!("opening {repository}#{number}");
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = Command::new("gh")
                .args(["issue", "view", &number.to_string(), "--web"])
                .args(["-R", &repository])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .await;
            let status = match result {
                Ok(o) if o.status.success() => format!("opened {repository}#{number}"),
                Ok(o) => format!("gh: {}", String::from_utf8_lossy(&o.stderr).trim()),
                Err(e) => format!("gh: {e}"),
            };
            let _ = tx.send(AppEvent::Status(status));
        });
    }

    fn scroll(&mut self, delta: i32) {
        self.scroll_pane(self.focus, delta);
    }

    fn scroll_pane(&mut self, pane: Focus, delta: i32) {
        let apply = |v: u16| (v as i32 + delta).max(0) as u16;
        match pane {
            Focus::Board => self.board_scroll = apply(self.board_scroll),
            Focus::Log => {
                // Detach from the bottom using the last rendered position
                let current = self.log_scroll.unwrap_or(self.log_bottom_hint);
                self.log_scroll = Some(apply(current));
            }
        }
    }

    fn on_received(&mut self, payload: Payload) {
        let hook_type = payload.hook_type();
        let issue = payload
            .issue_number
            .map(|n| format!("#{n} "))
            .unwrap_or_default();

        if !self.config.is_target(&payload.repository) {
            self.push(
                LogKind::Skipped,
                format!(
                    "⚡️ {issue}{hook_type} ({}: not in targets)",
                    payload.repository
                ),
            );
            return;
        }

        self.push(
            LogKind::Event,
            format!("⚡️ {issue}{hook_type} ({})", payload.repository),
        );

        let actions: Vec<HookAction> = hook::select(&self.config, &payload)
            .into_iter()
            .cloned()
            .collect();
        if !actions.is_empty() {
            let key = payload
                .issue_number
                .map(|n| issue_key(&payload.repository, n));
            if let Some(key) = &key {
                *self.active_issues.entry(key.clone()).or_default() += actions.len();
            }
            let tx = &self.tx;
            let _ = self
                .hook_workers
                .entry(key)
                .or_insert_with(|| spawn_hook_worker(tx.clone()))
                .send((payload, actions));
        }
        self.refresh_board();
    }

    fn on_hook_output(&mut self, tag: &str, output: HookOutput) {
        let (kind, text) = match output {
            HookOutput::Started(command) => (LogKind::Command, format!("$ {command}")),
            HookOutput::Stdout(line) => (LogKind::Stdout, format!("  {line}")),
            HookOutput::Stderr(line) => (LogKind::Stderr, format!("  {line}")),
        };
        self.push(kind, format!("  [{tag}] {text}"));
    }

    fn finish_actions(&mut self, issue: Option<&IssueKey>, count: usize) {
        if let Some(key) = issue
            && let Some(remaining) = self.active_issues.get_mut(key)
        {
            *remaining = remaining.saturating_sub(count);
            if *remaining == 0 {
                self.active_issues.remove(key);
            }
        }
    }

    fn push(&mut self, kind: LogKind, text: String) {
        self.log.push(LogLine { kind, text });
    }
}

/// Run hook actions of one issue sequentially so that the comment check and
/// actions keep the order of events. Workers of different issues run concurrently.
///
/// For issue events, actions run only when the issue status matches the
/// action's `status` (if specified), and then only when the last comment of
/// the issue changed since the previous check (or it is the first check).
/// The comment check is skipped when the issue has no comments.
fn spawn_hook_worker(tx: UnboundedSender<AppEvent>) -> UnboundedSender<HookJob> {
    let (hook_tx, mut hook_rx) = tokio::sync::mpsc::unbounded_channel::<HookJob>();
    tokio::spawn(async move {
        let mut tracker = CommentTracker::default();
        while let Some((payload, mut actions)) = hook_rx.recv().await {
            let issue = payload
                .issue_number
                .map(|n| issue_key(&payload.repository, n));
            let tag = match payload.issue_number {
                Some(n) => format!("#{n}"),
                None => payload.hook_type(),
            };
            let label = match payload.issue_number {
                Some(n) => format!("{} #{n}", payload.hook_type()),
                None => payload.hook_type(),
            };
            let skip = |count: usize, reason: String| {
                AppEvent::HookSkipped(issue.clone(), count, format!("{label}: {reason}"))
            };

            let state = match payload.issue_number {
                Some(n) => match hook::issue_state(&payload.repository, n).await {
                    Ok(state) => Some((n, state)),
                    Err(reason) => {
                        if tx.send(skip(actions.len(), reason)).is_err() {
                            return;
                        }
                        continue;
                    }
                },
                None => None,
            };

            // Status check first (events without issue have no status)
            let statuses = state
                .as_ref()
                .map(|(_, s)| s.statuses.clone())
                .unwrap_or_default();
            let (accepted, rejected) = actions
                .into_iter()
                .partition::<Vec<_>, _>(|action| action.accepts(&statuses));
            for action in &rejected {
                if tx
                    .send(skip(1, status_mismatch(action, &statuses)))
                    .is_err()
                {
                    return;
                }
            }
            actions = accepted;
            if actions.is_empty() {
                continue;
            }

            // Then comment check, only when the issue has a comment
            if let Some((n, state)) = state
                && state.comment.is_some()
                && !tracker.update(&payload.repository, n, state.comment)
            {
                if tx
                    .send(skip(actions.len(), "no new comment".to_string()))
                    .is_err()
                {
                    return;
                }
                continue;
            }

            for action in &actions {
                let status = hook::run(action, &payload, |output| {
                    let _ = tx.send(AppEvent::HookOutput(tag.clone(), output));
                })
                .await;
                if tx
                    .send(AppEvent::HookDone(issue.clone(), tag.clone(), status))
                    .is_err()
                {
                    return;
                }
            }
        }
    });
    hook_tx
}

fn status_mismatch(action: &HookAction, statuses: &[String]) -> String {
    let actual = if statuses.is_empty() {
        "none".to_string()
    } else {
        statuses.join(", ")
    };
    format!(
        "status is {actual} (expected: {})",
        action.status.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(repository: &str, event_name: &str) -> Payload {
        Payload::from_value(json!({
            "repository": repository,
            "event_name": event_name,
            "event_action": "opened",
            "issue_number": 3
        }))
        .unwrap()
    }

    fn app(config: &str) -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(Config::parse(config).unwrap(), tx)
    }

    #[tokio::test]
    async fn active_issue_is_cleared_after_actions_finish() {
        let mut app = app(r#"{
            "listen": "ws://x",
            "hooks": [
                { "on": ["issues:*"], "command": "true" },
                { "on": ["issues:*"], "command": "true" }
            ]
        }"#);
        let key = issue_key("Owner/Repo", 3);

        // no matching hook -> not marked
        app.handle(AppEvent::Received(payload("owner/repo", "issue_comment")));
        assert!(app.active_issues.is_empty());

        app.handle(AppEvent::Received(payload("Owner/Repo", "issues")));
        assert_eq!(app.active_issues.get(&key), Some(&2));
        app.handle(AppEvent::HookDone(Some(key.clone()), "#3".into(), Some(0)));
        assert_eq!(app.active_issues.get(&key), Some(&1));
        app.handle(AppEvent::HookDone(Some(key.clone()), "#3".into(), Some(0)));
        assert!(app.active_issues.is_empty());

        // skipped actions also clear the mark
        app.handle(AppEvent::Received(payload("owner/repo", "issues")));
        app.handle(AppEvent::HookSkipped(Some(key), 2, "no new comment".into()));
        assert!(app.active_issues.is_empty());
    }

    #[tokio::test]
    async fn ignore_repositories_not_in_targets() {
        let mut app = app(r#"{
            "listen": "ws://x",
            "targets": ["owner/repo"],
            "hooks": [{ "on": ["issues:*"], "command": "true" }]
        }"#);
        app.handle(AppEvent::Received(payload("owner/other", "issues")));
        assert!(app.active_issues.is_empty());
        assert_eq!(app.log.last().unwrap().kind, LogKind::Skipped);

        app.handle(AppEvent::Received(payload("owner/repo", "issues")));
        assert_eq!(app.active_issues.len(), 1);
    }
}
