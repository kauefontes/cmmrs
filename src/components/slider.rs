use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::styles;

const BAR_WIDTH: usize = 20;

/// A continuous (0..max) VCP control rendered as a bar, e.g. Brightness,
/// Contrast, RGB gain, Volume.
#[derive(Debug, Clone)]
pub struct Slider {
    pub code: u8,
    pub name: String,
    pub value: u16,
    pub max: u16,
    pub step: u16,
}

impl Slider {
    /// Builds a slider with a step size scaled to its range.
    pub fn new(code: u8, name: impl Into<String>, value: u16, max: u16) -> Self {
        let step = if max <= 20 { 1 } else { 5 };
        Slider {
            code,
            name: name.into(),
            value,
            max,
            step,
        }
    }

    pub fn view(&self, focused: bool) -> Line<'static> {
        let filled = if self.max > 0 {
            ((self.value as usize) * BAR_WIDTH / (self.max as usize)).min(BAR_WIDTH)
        } else {
            0
        };
        let bar_filled = "█".repeat(filled);
        let bar_empty = "░".repeat(BAR_WIDTH - filled);

        let (cursor, name_style) = if focused {
            ("▸ ", styles::focused_name())
        } else {
            ("  ", styles::name())
        };

        Line::from(vec![
            Span::raw(cursor),
            Span::styled(format!("{:<18}", self.name), name_style),
            Span::raw(" ["),
            Span::styled(bar_filled, Style::default().fg(styles::ACCENT)),
            Span::styled(bar_empty, Style::default().fg(styles::DIM)),
            Span::raw(format!("] {:>3}", self.value)),
        ])
    }
}
