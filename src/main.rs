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
use crate::config::{CONFIG_FILE_NAME, Config};

const USAGE: &str = "usage: gh-board-cli <owner>/<repo>";

#[tokio::main]
async fn main() -> Result<()> {
    let repository = parse_args()?;
    let path = Config::find().with_context(|| format!("{CONFIG_FILE_NAME} not found"))?;
    let config = Config::load(&path)?;

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

    let mut app = App::new(repository, config, tx);
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
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn parse_args() -> Result<String> {
    let mut args = std::env::args().skip(1);
    let (Some(repository), None) = (args.next(), args.next()) else {
        bail!(USAGE);
    };
    match repository.split_once('/') {
        Some((owner, repo)) if !owner.is_empty() && !repo.is_empty() && !repo.contains('/') => {
            Ok(repository)
        }
        _ => bail!(USAGE),
    }
}
