mod app;
mod aws;
mod config;
mod event;
mod tunnel;
mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::Event;
use tokio::sync::mpsc;

use app::App;

#[tokio::main]
async fn main() -> Result<()> {
    let mut app = match App::new() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("moleman: {e:#}");
            std::process::exit(1);
        }
    };

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app).await;
    ratatui::restore();

    if let Err(e) = result {
        eprintln!("moleman: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    // Best-effort initial discovery so the Services group reflects live state.
    app.refresh().await;

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    spawn_input(tx);

    let mut tick = tokio::time::interval(Duration::from_millis(1000));

    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(Event::Key(key)) => event::handle_key(app, key).await,
                    Some(_) => {} // resize/mouse: next draw handles it
                    None => app.should_quit = true, // input thread gone
                }
            }
            _ = tick.tick() => {
                app.on_tick().await;
            }
        }

        if app.should_quit {
            app.shutdown().await;
            break;
        }
    }

    Ok(())
}

/// Crossterm reads are blocking, so they live on a dedicated thread that feeds
/// events into the async loop over a channel.
fn spawn_input(tx: mpsc::UnboundedSender<Event>) {
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
}
