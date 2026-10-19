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
use crate::hook::{self, CommandOutput};
use crate::payload::Payload;

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
    /// Hook action finished (issue number of the event, output)
    HookDone(Option<u64>, CommandOutput),
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
    pub repository: String,
    config: Config,
    tx: UnboundedSender<AppEvent>,
    hook_tx: UnboundedSender<HookJob>,

    pub board: Option<Board>,
    pub board_error: Option<String>,
    pub loading: bool,
    /// Refresh requested while loading
    refresh_pending: bool,
    /// Issue number -> count of hook actions not yet finished
    pub active_issues: HashMap<u64, usize>,
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
    pub issue_links: Vec<(Rect, u64)>,
    pub quit: bool,
}

type HookJob = (Payload, Vec<HookAction>);

impl App {
    pub fn new(repository: String, config: Config, tx: UnboundedSender<AppEvent>) -> Self {
        let hook_tx = spawn_hook_worker(tx.clone());
        Self {
            repository,
            config,
            tx,
            hook_tx,
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
        let repository = self.repository.clone();
        tokio::spawn(async move {
            let result = board::fetch(&repository)
                .await
                .map_err(|e| format!("{e:#}"));
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
            AppEvent::HookDone(issue, output) => self.on_hook_done(issue, output),
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
                if let Some(&(_, number)) = link {
                    self.open_issue(number);
                }
            }
            MouseEventKind::ScrollDown => self.scroll_pane(pane, 1),
            MouseEventKind::ScrollUp => self.scroll_pane(pane, -1),
            _ => {}
        }
    }

    fn open_issue(&mut self, number: u64) {
        self.status = format!("opening #{number}");
        let tx = self.tx.clone();
        let repository = self.repository.clone();
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
                Ok(o) if o.status.success() => format!("opened #{number}"),
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

        if !payload.repository.eq_ignore_ascii_case(&self.repository) {
            self.push(
                LogKind::Skipped,
                format!("⚡️ {issue}{hook_type} ({}: skipped)", payload.repository),
            );
            return;
        }

        self.push(LogKind::Event, format!("⚡️ {issue}{hook_type}"));

        let actions: Vec<HookAction> = hook::select(&self.config, &payload)
            .into_iter()
            .cloned()
            .collect();
        if !actions.is_empty() {
            if let Some(n) = payload.issue_number {
                *self.active_issues.entry(n).or_default() += actions.len();
            }
            let _ = self.hook_tx.send((payload, actions));
        }
        self.refresh_board();
    }

    fn on_hook_done(&mut self, issue: Option<u64>, output: CommandOutput) {
        if let Some(n) = issue
            && let Some(count) = self.active_issues.get_mut(&n)
        {
            *count -= 1;
            if *count == 0 {
                self.active_issues.remove(&n);
            }
        }
        self.push(LogKind::Command, format!("  $ {}", output.command));
        for line in output.stdout.lines() {
            self.push(LogKind::Stdout, format!("    {line}"));
        }
        for line in output.stderr.lines() {
            self.push(LogKind::Stderr, format!("    {line}"));
        }
        let status = output
            .status
            .map_or("killed".to_string(), |c| c.to_string());
        self.push(LogKind::Exit, format!("    exit {status}"));
    }

    fn push(&mut self, kind: LogKind, text: String) {
        self.log.push(LogLine { kind, text });
    }
}

/// Run hook actions sequentially so that outputs keep the order of events.
fn spawn_hook_worker(tx: UnboundedSender<AppEvent>) -> UnboundedSender<HookJob> {
    let (hook_tx, mut hook_rx) = tokio::sync::mpsc::unbounded_channel::<HookJob>();
    tokio::spawn(async move {
        while let Some((payload, actions)) = hook_rx.recv().await {
            for action in &actions {
                let output = hook::run(action, &payload).await;
                let done = AppEvent::HookDone(payload.issue_number, output);
                if tx.send(done).is_err() {
                    return;
                }
            }
        }
    });
    hook_tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output() -> CommandOutput {
        CommandOutput {
            command: "true".into(),
            stdout: String::new(),
            stderr: String::new(),
            status: Some(0),
        }
    }

    #[tokio::test]
    async fn active_issue_is_cleared_after_actions_finish() {
        let config = Config::parse(
            r#"{
                "listen": "ws://x",
                "hooks": {
                    "issues:*": [
                        { "type": "command", "command": "true" },
                        { "type": "command", "command": "true" }
                    ]
                }
            }"#,
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new("owner/repo".into(), config, tx);

        let payload = |event_name: &str| {
            Payload::from_value(json!({
                "repository": "owner/repo",
                "event_name": event_name,
                "event_action": "opened",
                "issue_number": 3
            }))
            .unwrap()
        };

        // no matching hook -> not marked
        app.handle(AppEvent::Received(payload("issue_comment")));
        assert!(app.active_issues.is_empty());

        app.handle(AppEvent::Received(payload("issues")));
        assert_eq!(app.active_issues.get(&3), Some(&2));
        app.handle(AppEvent::HookDone(Some(3), output()));
        assert_eq!(app.active_issues.get(&3), Some(&1));
        app.handle(AppEvent::HookDone(Some(3), output()));
        assert!(app.active_issues.is_empty());
    }
}
