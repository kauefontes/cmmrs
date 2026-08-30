use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::components::{truncate_name, NAME_WIDTH};
use crate::styles;

const BAR_WIDTH: usize = 20;

/// Column offset, within this control's own rendered line, where the
/// bar's fill characters start — the cursor marker (`"▸ "`/`"  "`, 2
/// cols) + the name field (`{:<NAME_WIDTH}`) + `" ▐"` (2 cols) in
/// `view()`. Kept here, right next to `BAR_WIDTH`, specifically so a
/// layout tweak to `view()` can't silently desync it from
/// `value_at_column` — if you change one, the other is right there.
const BAR_START_COL: u16 = 2 + NAME_WIDTH as u16 + 2;

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
        // Medium/light shade fill between half-block end caps — reads
        // like the groove a physical monitor's own OSD bar sits in,
        // rather than a plain bracketed `[###...]` progress bar.
        let bar_filled = "▓".repeat(filled);
        let bar_empty = "░".repeat(BAR_WIDTH - filled);

        let (cursor, name_style) = if focused {
            ("▸ ", styles::focused_name())
        } else {
            ("  ", styles::name())
        };

        let line = Line::from(vec![
            Span::raw(cursor),
            Span::styled(format!("{:<NAME_WIDTH$}", truncate_name(&self.name, NAME_WIDTH)), name_style),
            Span::raw(" ▐"),
            Span::styled(bar_filled, Style::default().fg(styles::ACCENT)),
            Span::styled(bar_empty, Style::default().fg(styles::DIM)),
            Span::raw(format!("▌ {:>3}", self.value)),
        ]);
        if focused {
            styles::with_focus_bg(line)
        } else {
            line
        }
    }

    /// The value a click/drag at `col` (0-based, relative to the start of
    /// this control's own rendered line — see `App::click_origin_col`)
    /// would set, or `None` if `col` falls outside the bar itself (the
    /// name label before it, or the `"] NNN"` value text after it) —
    /// callers use that to tell "clicked the bar" from "clicked the row
    /// but not the bar", which should only focus, not also set a value.
    ///
    /// Maps linearly so the bar's *edges* hit the range's exact
    /// endpoints (leftmost cell -> 0, rightmost cell -> `max`) rather
    /// than a cell's center — the usual feel for a slider: drag to
    /// either end and you actually reach it, no dead zone.
    pub fn value_at_column(&self, col: u16) -> Option<u16> {
        let offset = col.checked_sub(BAR_START_COL)?;
        if offset as usize >= BAR_WIDTH {
            return None;
        }
        if self.max == 0 || BAR_WIDTH == 1 {
            return Some(0);
        }
        let denom = (BAR_WIDTH - 1) as u32;
        let numerator = offset as u32 * self.max as u32;
        // Round to nearest instead of truncating.
        let value = (numerator + denom / 2) / denom;
        Some(value.min(self.max as u32) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slider(max: u16) -> Slider {
        Slider::new(0x10, "Test", 0, max)
    }

    #[test]
    fn leftmost_bar_cell_is_zero() {
        assert_eq!(slider(100).value_at_column(BAR_START_COL), Some(0));
    }

    #[test]
    fn rightmost_bar_cell_is_max() {
        assert_eq!(slider(100).value_at_column(BAR_START_COL + BAR_WIDTH as u16 - 1), Some(100));
    }

    #[test]
    fn before_the_bar_is_none() {
        let s = slider(100);
        assert_eq!(s.value_at_column(0), None);
        assert_eq!(s.value_at_column(BAR_START_COL - 1), None);
    }

    #[test]
    fn after_the_bar_is_none() {
        assert_eq!(slider(100).value_at_column(BAR_START_COL + BAR_WIDTH as u16), None);
    }

    #[test]
    fn midpoint_is_roughly_half_max() {
        // BAR_WIDTH is 20, so column index 9 or 10 (of 0..19) is close to
        // the middle of a 0-based, edge-inclusive 0..19 scale.
        let v = slider(100).value_at_column(BAR_START_COL + 9).unwrap();
        assert!((40..=60).contains(&v), "expected roughly half of 100, got {v}");
    }

    #[test]
    fn zero_max_never_panics_and_is_always_zero() {
        assert_eq!(slider(0).value_at_column(BAR_START_COL), Some(0));
    }

    /// `BAR_START_COL` is a hand-counted constant tracking `view()`'s
    /// layout — this renders a real `Slider` through ratatui's actual
    /// pipeline (unicode cell widths and all, not `str::chars().count()`,
    /// which could quietly disagree for a wide character) and checks the
    /// bar lands exactly where the constant says, so a future `view()`
    /// tweak that forgets to update it fails loudly here instead of
    /// silently mis-mapping every click.
    #[test]
    fn bar_start_col_matches_the_real_rendered_layout() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        use ratatui::widgets::Paragraph;
        use ratatui::Terminal;

        let s = Slider::new(0x10, "Brightness", 50, 100);
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|f| f.render_widget(Paragraph::new(s.view(false)), Rect::new(0, 0, 60, 1)))
            .unwrap();
        let buf = terminal.backend().buffer();
        let bar_col = (0..60)
            .find(|&x| matches!(buf[(x, 0)].symbol(), "▓" | "░"))
            .expect("expected the bar's fill characters somewhere in the rendered row");
        assert_eq!(bar_col, BAR_START_COL);
    }
}
