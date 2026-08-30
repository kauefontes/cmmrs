//! Multi-display picker screen — port of `picker.go`.

use ratatui::text::{Line, Span};

use crate::app::App;
use crate::styles;

pub fn render(app: &App) -> Vec<Line<'static>> {
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
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "↑↓ navigate · enter select · q quit",
        styles::dim(),
    ));
    lines
}
