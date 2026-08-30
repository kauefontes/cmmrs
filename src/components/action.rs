use ratatui::text::{Line, Span};

use crate::styles;

/// A write-only, non-continuous VCP command — there's nothing to read back,
/// just a code to trigger (e.g. "Restore factory defaults").
#[derive(Debug, Clone)]
pub struct Action {
    pub code: u8,
    pub name: String,
}

impl Action {
    pub fn view(&self, focused: bool) -> Line<'static> {
        let (cursor, style) = if focused {
            ("▸ ", styles::focused_name())
        } else {
            ("  ", styles::name())
        };
        let line = Line::from(vec![
            Span::raw(cursor),
            Span::styled(format!("[ {} ]", self.name), style),
        ]);
        if focused {
            styles::with_focus_bg(line)
        } else {
            line
        }
    }
}
