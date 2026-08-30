//! Small reusable pieces of the controls screen — a DDC/CI brightness-style
//! slider isn't really a generic "progress" widget, so these are rolled by
//! hand, same as the Go original's `internal/tui/components`.

mod action;
mod selector;
mod slider;

pub use action::Action;
pub use selector::{Option as SelectorOption, Selector};
pub use slider::Slider;

/// Shared name-column width for `Slider`/`Selector`'s `view()` — kept in
/// one place so the two stay visually aligned with each other on screen
/// (an action doesn't pad its name; nothing sits to the right of it that
/// alignment would matter for).
pub(crate) const NAME_WIDTH: usize = 22;

/// Truncates `name` to at most `width` chars, appending `…` if it had
/// to — keeps the name column a predictable, aligned width regardless of
/// what a real monitor happens to call something (some VCP names, e.g.
/// "Select Color Preset", run longer than you'd guess).
pub(crate) fn truncate_name(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        name.to_string()
    } else {
        let mut s: String = name.chars().take(width.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}
