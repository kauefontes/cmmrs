mod app;
mod backend;
mod cache;
mod categories;
mod commands;
mod components;
mod logging;
mod screens;
mod styles;
mod ui;
mod vcp;
mod worker;

use std::io;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use backend::ddcutil::DdcutilBackend;
use backend::DdcBackend;
use worker::Worker;

fn main() -> io::Result<()> {
    logging::init();

    let mut terminal = setup_terminal()?;

    let result = run(&mut terminal);

    restore_terminal(&mut terminal)?;

    if let Err(e) = result {
        log::error!("fatal: {e}");
        eprintln!("error: {e}");
        std::process::exit(1);
    }
    log::info!("cmmrs exiting normally");
    Ok(())
}

/// Tries the native backend first (no `ddcutil` subprocess, no external
/// dependency), falling back to shelling out to `ddcutil` where no native
/// backend exists yet (or it found nothing) — see `backend::mod`.
fn pick_backend() -> Arc<dyn DdcBackend> {
    #[cfg(target_os = "linux")]
    {
        let native = backend::native::NativeBackend::new();
        match native.detect() {
            Ok(displays) if !displays.is_empty() => {
                log::info!("using native DDC/CI backend ({} display(s) found)", displays.len());
                return Arc::new(native);
            }
            Ok(_) => log::info!("native DDC/CI backend found no displays, falling back to ddcutil"),
            Err(e) => log::warn!("native DDC/CI backend unavailable, falling back to ddcutil: {e}"),
        }
    }
    log::info!("using ddcutil backend");
    Arc::new(DdcutilBackend::new())
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let backend = pick_backend();
    let (tx, rx) = mpsc::channel();
    // One worker thread for every backend call, so DDC/i2c access to the
    // monitor is always serialized — see `worker`'s module docs.
    let worker = Worker::new();

    let mut app = App::new();
    commands::dispatch(App::init(), &backend, &tx, &worker);

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        if app.should_quit {
            break;
        }

        // Drain any async results first so a burst of them (e.g. the
        // live-value reads after a cache hit, now arriving one at a time
        // off the worker queue) doesn't wait on a terminal event to be seen.
        while let Ok(msg) = rx.try_recv() {
            if let Some(cmd) = app.handle_msg(msg) {
                commands::dispatch(cmd, &backend, &tx, &worker);
            }
        }

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
                    if let Some(cmd) = app.handle_key(key) {
                        commands::dispatch(cmd, &backend, &tx, &worker);
                    }
                }
                Event::Mouse(mouse) => {
                    if let Some(cmd) = app.handle_mouse(mouse) {
                        commands::dispatch(cmd, &backend, &tx, &worker);
                    }
                }
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
