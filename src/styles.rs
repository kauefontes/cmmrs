//! Shared colors/styles, mirroring the Go project's `styles.go` (Lip Gloss)
//! 1:1 — same ANSI 256 palette indices, so it looks identical in a
//! terminal.

use ratatui::style::{Color, Modifier, Style};

pub const BORDER_COLOR: Color = Color::Indexed(62);
pub const ACCENT: Color = Color::Indexed(212);
pub const DIM: Color = Color::Indexed(240);
pub const OK_COLOR: Color = Color::Indexed(42);
pub const ERR_COLOR: Color = Color::Indexed(196);

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

/// Highlights the focused row on the Raw VCP screen.
pub fn raw_focused() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}
