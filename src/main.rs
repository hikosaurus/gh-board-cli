mod app;
mod board;
mod config;
mod hook;
mod listener;
mod payload;
mod ui;

use anyhow::{Context, Result, bail};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
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

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = async {
        while !app.quit {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            let Some(event) = rx.recv().await else {
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
