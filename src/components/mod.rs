//! Small reusable pieces of the controls screen — a DDC/CI brightness-style
//! slider isn't really a generic "progress" widget, so these are rolled by
//! hand, same as the Go original's `internal/tui/components`.

mod action;
mod selector;
mod slider;

pub use action::Action;
pub use selector::{Option as SelectorOption, Selector};
pub use slider::Slider;
