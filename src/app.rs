use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::board::{self, Board};
use crate::config::{Config, HookAction};
use crate::hook::{self, CommandOutput};
use crate::payload::Payload;

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    /// Terminal event other than key (e.g. resize)
    Redraw,
    /// WebSocket connection status
    Status(String),
    /// Payload received from WebSocket
    Received(Payload),
    HookDone(CommandOutput),
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
    hook_tx: UnboundedSender<(Payload, Vec<HookAction>)>,

    pub board: Option<Board>,
    pub board_error: Option<String>,
    pub loading: bool,
    /// Refresh requested while loading
    refresh_pending: bool,
    /// Issues which received events
    pub active_issues: HashSet<u64>,
    pub log: Vec<LogLine>,
    pub status: String,

    pub focus: Focus,
    pub board_scroll: u16,
    /// `None` follows the latest log
    pub log_scroll: Option<u16>,
    /// Scroll position of the bottom of the log, updated by the renderer
    pub log_bottom_hint: u16,
    pub quit: bool,
}

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
            active_issues: HashSet::new(),
            log: vec![],
            status: String::new(),
            focus: Focus::Board,
            board_scroll: 0,
            log_scroll: None,
            log_bottom_hint: 0,
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
            AppEvent::Redraw => {}
            AppEvent::Status(status) => self.status = status,
            AppEvent::Received(payload) => self.on_received(payload),
            AppEvent::HookDone(output) => self.on_hook_done(output),
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

    fn scroll(&mut self, delta: i32) {
        let apply = |v: u16| (v as i32 + delta).max(0) as u16;
        match self.focus {
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
            .issue_number()
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
        if let Some(n) = payload.issue_number() {
            self.active_issues.insert(n);
        }

        let actions: Vec<HookAction> = hook::select(&self.config, &payload)
            .into_iter()
            .cloned()
            .collect();
        if !actions.is_empty() {
            let _ = self.hook_tx.send((payload, actions));
        }
        self.refresh_board();
    }

    fn on_hook_done(&mut self, output: CommandOutput) {
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
fn spawn_hook_worker(tx: UnboundedSender<AppEvent>) -> UnboundedSender<(Payload, Vec<HookAction>)> {
    let (hook_tx, mut hook_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Payload, Vec<HookAction>)>();
    tokio::spawn(async move {
        while let Some((payload, actions)) = hook_rx.recv().await {
            for action in &actions {
                let output = hook::run(action, &payload).await;
                if tx.send(AppEvent::HookDone(output)).is_err() {
                    return;
                }
            }
        }
    });
    hook_tx
}
