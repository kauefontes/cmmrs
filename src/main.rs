mod app;
mod backend;
mod cache;
mod categories;
mod commands;
mod components;
mod effects;
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
    #[cfg(target_os = "macos")]
    {
        let native = backend::macos::MacosBackend::new();
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

/// Idle poll timeout — plenty responsive for keyboard/mouse input at
/// rest, and how often a loading spinner (see `ui::spinner`) effectively
/// advances when nothing else is happening.
const IDLE_POLL: Duration = Duration::from_millis(50);
/// Poll timeout while a tachyonfx effect (`app.effects`) is actually
/// running — shorter so a fade/flash gets enough frames to look smooth
/// rather than choppy. Only paid while something's actually animating.
const ANIMATING_POLL: Duration = Duration::from_millis(16);
/// Ceiling on the `elapsed` duration ever fed to `process_effects` in one
/// step. Without this, the first frame after triggering an effect — say,
/// a screen switch — advances it by however long the app sat idle
/// *waiting for the keypress that triggered it*, not by an actual frame
/// interval: a user pausing a second before pressing `v` would hand a
/// ~250ms transition effect a single ~1000ms step, which finishes it
/// before it's ever had a second frame to animate across — reading as a
/// flash, not a transition. Capping each step at this (comfortably above
/// `ANIMATING_POLL`) keeps every effect's first real step small no matter
/// how long the app was idle beforehand.
const MAX_EFFECT_STEP: Duration = Duration::from_millis(33);

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let backend = pick_backend();
    let (tx, rx) = mpsc::channel();
    // One worker thread for every backend call, so DDC/i2c access to the
    // monitor is always serialized — see `worker`'s module docs.
    let worker = Worker::new();

    let mut app = App::new();
    commands::dispatch(App::init(), &backend, &tx, &worker);

    let mut last_frame = std::time::Instant::now();

    loop {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(last_frame).min(MAX_EFFECT_STEP);
        last_frame = now;

        terminal.draw(|f| {
            ui::draw(f, &mut app);
            // Effects paint *over* whatever ui::draw just rendered, so
            // this has to run after it, in the same frame — see
            // App::effects' docs. `area` first: `buffer_mut()` borrows
            // `f` mutably, so it has to be the last thing taken from it.
            let area = f.area();
            app.effects.process_effects(elapsed.into(), f.buffer_mut(), area);
        })?;

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

        let poll_timeout = if app.effects.is_running() { ANIMATING_POLL } else { IDLE_POLL };
        if event::poll(poll_timeout)? {
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


/// End-to-end effect tests exercising the real pipeline this module's
/// `run()` drives (App queues an effect -> process_effects renders it
/// into a real buffer over several frames), rather than just checking
/// `App::effects.is_running()` flips true (already covered in
/// `app.rs`'s own test suite). Each test here pins down one thing that
/// was visibly wrong before this module's `MAX_EFFECT_STEP` fix and
/// `effects::materialize` effect landed.
#[cfg(test)]
mod effects_integration_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn app_with_content() -> App {
        let mut app = App::new();
        app.loading = false;
        app.displays = vec![crate::vcp::Display { number: 1, mfg_id: "GSM".into(), model: "LG ULTRAWIDE".into(), ..Default::default() }];
        app.caps = Some(crate::vcp::Capabilities { model: String::new(), mccs_version: "2.1".into(), features: vec![] });
        app
    }

    #[test]
    fn materialize_shows_noise_then_resolves_to_real_text() {
        let mut app = app_with_content();
        app.handle_msg(commands::Msg::Probe(Ok(commands::ProbeOk {
            caps: crate::vcp::Capabilities { model: String::new(), mccs_version: "2.1".into(), features: vec![] },
            sliders: vec![], selectors: vec![], actions: vec![],
            order: vec![],
        })));
        assert!(app.effects.is_running());

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut saw_noise = false;
        for _ in 0..30 {
            terminal.draw(|f| {
                ui::draw(f, &mut app);
                let area = f.area();
                app.effects.process_effects(std::time::Duration::from_millis(16).into(), f.buffer_mut(), area);
            }).unwrap();
            let buf = terminal.backend().buffer().clone();
            let row2: String = (0..40).map(|x| buf[(x, 2)].symbol().to_string()).collect();
            if row2.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)) {
                saw_noise = true;
            }
        }
        assert!(saw_noise, "expected braille noise glyphs during the animation");
        assert!(!app.effects.is_running(), "effect should have completed");

        let buf = terminal.backend().buffer().clone();
        let final_row2: String = (0..40).map(|x| buf[(x, 2)].symbol().to_string()).collect();
        assert!(!final_row2.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "no noise glyphs should remain after completion: {final_row2:?}");
    }

    #[test]
    fn a_long_idle_gap_does_not_finish_the_effect_in_one_frame() {
        let mut app = app_with_content();
        app.handle_msg(commands::Msg::Probe(Ok(commands::ProbeOk {
            caps: crate::vcp::Capabilities { model: String::new(), mccs_version: "2.1".into(), features: vec![] },
            sliders: vec![], selectors: vec![], actions: vec![],
            order: vec![],
        })));

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        // Simulate exactly what a 1.5s idle gap before the triggering
        // keypress would otherwise hand process_effects, clamped the
        // same way main's loop clamps it.
        let elapsed = std::time::Duration::from_millis(1500).min(MAX_EFFECT_STEP);
        terminal.draw(|f| {
            ui::draw(f, &mut app);
            let area = f.area();
            app.effects.process_effects(elapsed.into(), f.buffer_mut(), area);
        }).unwrap();

        assert!(app.effects.is_running(), "a clamped step from a long idle gap must not finish the effect in one frame");
    }
}
