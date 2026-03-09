mod app;
mod ui;

use anyhow::Result;
use app::{App, AppMessage};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::Duration};
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(
    name = "adsb-terminal",
    about = "Terminal dashboard for the ADS-B Router — shows per-source frame throughput and emergency events"
)]
struct Args {
    /// gRPC address of the running adsb-router
    #[arg(short, long, default_value = "http://localhost:50051")]
    addr: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(args.addr);
    let (tx, rx) = mpsc::channel(512);
    app.start_grpc_client(tx);

    let res = run_app(&mut terminal, &mut app, rx).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut rx: mpsc::Receiver<AppMessage>,
) -> Result<()> {
    loop {
        while let Ok(msg) = rx.try_recv() {
            app.handle_message(msg);
        }

        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                        return Ok(());
                    }
                }
            }
        }
    }
}
