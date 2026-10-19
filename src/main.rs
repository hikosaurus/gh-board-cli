mod app;
mod board;
mod config;
mod hook;
mod listener;
mod payload;
mod ui;

use anyhow::{Context, Result, bail};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::{cursor, execute};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::app::{App, AppEvent};
use crate::config::{CONFIG_FILE_NAMES, Config};

const USAGE: &str = "usage: gh-board-cli";

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().len() > 1 {
        bail!(USAGE);
    }
    let path =
        Config::find().with_context(|| format!("{} not found", CONFIG_FILE_NAMES.join(" or ")))?;
    let config = Config::load(&path)?;

    // Select the TLS crypto provider for `wss://` explicitly.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (tx, mut rx) = mpsc::unbounded_channel();

    tokio::spawn(listener::listen(
        config.listen.clone(),
        config.channel.clone(),
        tx.clone(),
    ));

    let input_tx = tx.clone();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            let event = match ev {
                Event::Key(key) => AppEvent::Key(key),
                Event::Mouse(mouse) => AppEvent::Mouse(mouse),
                _ => AppEvent::Redraw,
            };
            if input_tx.send(event).is_err() {
                break;
            }
        }
    });

    let mut app = App::new(config, tx);
    app.refresh_board();

    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    install_panic_hook();
    let result = async {
        while !app.quit {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            let event = tokio::select! {
                event = rx.recv() => event,
                _ = sigint.recv() => None,
                _ = sigterm.recv() => None,
                _ = sighup.recv() => None,
            };
            let Some(event) = event else {
                break;
            };
            app.handle(event);
            // Handle queued events (e.g. bursts of hook output) before redrawing
            // so that drawing does not delay key input.
            while !app.quit
                && let Ok(event) = rx.try_recv()
            {
                app.handle(event);
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Restore the terminal and exit the process on a panic in any thread or task.
///
/// Without exiting, a panic in a spawned task restores the terminal (by the
/// hook installed by `ratatui::init`) while the app keeps running, so input is
/// echoed to the screen.
fn install_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture, cursor::Show);
        hook(info);
        std::process::exit(101);
    }));
}
