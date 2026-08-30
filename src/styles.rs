//! Shared colors/styles — a cyan/blue "screen glow" identity, deliberately
//! not the Go original's 1:1 ANSI-256 port anymore (see the visual redesign
//! this module is part of). Truecolor throughout: virtually every terminal
//! in real use renders 24-bit color today, and a cohesive palette needs the
//! full range, not the 256-color cube's coarse steps.
//!
//! Body text is left at the terminal's own default (`Color::Reset`) rather
//! than an explicit color — only accents, borders, dim text, and the
//! focused-row highlight below get a color from this palette. That's a
//! deliberate choice, not an oversight: painting a color over every line
//! would fight whatever foreground the user's own terminal theme already
//! provides (light or dark), and this app doesn't own the whole screen the
//! way a full "themed" TUI might.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

pub const BORDER_COLOR: Color = Color::Rgb(0x16, 0x4E, 0x63); // cyan-900
pub const ACCENT: Color = Color::Rgb(0x22, 0xD3, 0xEE); // cyan-400
pub const DIM: Color = Color::Rgb(0x64, 0x74, 0x8B); // slate-500
pub const OK_COLOR: Color = Color::Rgb(0x34, 0xD3, 0x99); // emerald-400
pub const ERR_COLOR: Color = Color::Rgb(0xF8, 0x71, 0x71); // red-400
/// Background tint for the focused row's full line — a physical monitor's
/// OSD highlights the *whole* selected row, not just the label, which is
/// what this is for (see `components::Slider`/`Selector`/`Action::view`).
pub const FOCUSED_BG: Color = Color::Rgb(0x0E, 0x3A, 0x45); // dark cyan tint

pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn ok() -> Style {
    Style::default().fg(OK_COLOR)
}

pub fn err() -> Style {
    Style::default().fg(ERR_COLOR)
}

pub fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn name() -> Style {
    Style::default()
}

pub fn focused_name() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Applies `FOCUSED_BG` across every span of `line` — used to highlight a
/// focused control's *entire* row (a physical OSD's convention — see
/// `FOCUSED_BG`'s docs), not just the name span `focused_name()` already
/// colors. Call this on the finished `Line` a `view()` builds, rather
/// than threading a background through each `Span` by hand.
pub fn with_focus_bg(mut line: Line<'static>) -> Line<'static> {
    for span in &mut line.spans {
        span.style = span.style.bg(FOCUSED_BG);
    }
    line
}

/// Highlights the focused row on the Raw VCP screen.
pub fn raw_focused() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// A category header on the Controls screen (`DISPLAY`, `AUDIO`, ...) —
/// see `categories.rs`.
pub fn section() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}
