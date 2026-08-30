//! Multi-display picker screen — port of `picker.go`.

use ratatui::text::{Line, Span};

use crate::app::{App, ClickTarget};
use crate::styles;

/// Renders the screen, plus a click target per line — see
/// `App::click_targets`'s docs.
pub fn render(app: &App) -> (Vec<Line<'static>>, Vec<Option<ClickTarget>>) {
    let mut lines = vec![
        Line::styled(
            format!(
                "{} DDC/CI displays found — choose one to control:",
                app.displays.len()
            ),
            styles::dim(),
        ),
        Line::raw(""),
    ];
    let mut targets = vec![None, None];

    for (i, d) in app.displays.iter().enumerate() {
        let mut label = d.mfg_id.clone();
        if !d.model.is_empty() {
            label.push(' ');
            label.push_str(&d.model);
        }
        label.push_str(&format!(" ({}, VCP {})", d.bus, d.vcp_version));

        let (cursor, style) = if i == app.picker_cursor {
            ("▸ ", styles::raw_focused())
        } else {
            ("  ", styles::dim())
        };
        lines.push(Line::from(vec![
            Span::raw(cursor),
            Span::styled(label, style),
        ]));
        targets.push(Some(ClickTarget::Display(i)));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "↑↓ navigate · enter/click select · scroll · q quit",
        styles::dim(),
    ));
    targets.push(None);
    targets.push(None);
    (lines, targets)
}
