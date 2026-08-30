//! Raw VCP screen — port of `rawview.go`. Shows every declared feature,
//! including the unrecognized/manufacturer-specific ones the controls
//! screen never surfaces, and supports editing a raw value by code.
//!
//! Unlike the rest of the app (plain `Vec<Line>` wrapped by
//! `ui::render_box`), the feature list itself renders as a real
//! `ratatui::widgets::Table` with its own `TableState` — that's what gives
//! it actual scrolling: ratatui keeps the selected row in view by
//! adjusting the table's scroll offset at render time, the same mechanism
//! every ratatui list/table screen uses, rather than a hand-tracked
//! `App`-side offset. `App::raw_table_state` just carries that offset
//! between frames; nothing in `App`'s update logic needs to know where the
//! viewport currently sits.

use std::collections::HashMap;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Padding, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table,
};
use ratatui::Frame;

use crate::app::App;
use crate::styles;
use crate::ui;
use crate::vcp::{Capabilities, FeatureReading};

/// Column widths for the feature table, shared between the real render and
/// the test that renders it into an off-screen buffer.
const WIDTHS: [Constraint; 4] = [
    Constraint::Length(4),
    Constraint::Length(34),
    Constraint::Length(13),
    Constraint::Min(10),
];

/// Draws the whole Raw VCP screen into `area`. Delegates to the shared
/// text-box chrome (`ui::render_box`) for every sub-state that's just
/// flowing text — loading, an error, the edit prompt, the write
/// confirmation — and only takes the real-`Rect`/stateful-widget path for
/// the actual scrollable feature table.
pub fn draw(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    if app.raw_confirming {
        ui::render_box(frame, area, ui::TITLE, render_confirm(app));
        return;
    }
    if app.raw_editing {
        ui::render_box(frame, area, ui::TITLE, render_edit(app));
        return;
    }
    let Some(caps) = app.caps.clone() else {
        ui::render_box(
            frame,
            area,
            ui::TITLE,
            vec![Line::styled("No capabilities loaded.", styles::err())],
        );
        return;
    };
    if app.raw_loading {
        let lines = vec![
            Line::styled(
                format!("Raw VCP — {} features declared", caps.features.len()),
                styles::dim(),
            ),
            Line::raw(""),
            Line::styled(
                "Scanning all VCP codes — this takes a few seconds...",
                styles::dim(),
            ),
        ];
        ui::render_box(frame, area, ui::TITLE, lines);
        return;
    }
    if let Some(e) = app.raw_err.clone() {
        let lines = vec![
            Line::styled(
                format!("Raw VCP — {} features declared", caps.features.len()),
                styles::dim(),
            ),
            Line::raw(""),
            Line::styled(format!("Error: {e}"), styles::err()),
        ];
        ui::render_box(frame, area, ui::TITLE, lines);
        return;
    }

    draw_table(frame, area, &caps, app);
}

/// The scrollable feature table itself: shared border/title chrome built
/// by hand (rather than through `ui::render_box`, which only knows how to
/// wrap a flat `Vec<Line>`) so the table gets a real, correctly-sized
/// `Rect` to render a stateful widget into.
fn draw_table(frame: &mut Frame<'_>, area: Rect, caps: &Capabilities, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(ratatui::style::Style::default().fg(styles::BORDER_COLOR))
        .padding(Padding::new(4, 4, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [title_area, _, caption_area, _, table_area, status_area, help_area] = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // blank
        Constraint::Length(1), // "Raw VCP — N features declared"
        Constraint::Length(1), // blank
        Constraint::Min(3),    // the table (its own header row eats one line of this)
        Constraint::Length(1), // write status ("Writing..." / an error)
        Constraint::Length(1), // help
    ])
    .areas(inner);

    frame.render_widget(
        Line::styled(ui::TITLE, styles::title()).alignment(Alignment::Center),
        title_area,
    );
    frame.render_widget(
        Line::styled(
            format!("Raw VCP — {} features declared", caps.features.len()),
            styles::dim(),
        ),
        caption_area,
    );

    // The table's own header row eats one line of `table_area`, so that's
    // how many *data* rows are actually visible — `App::page_raw` (f/b)
    // needs this to know how far a page-jump goes, the one piece of the
    // old hand-rolled viewport that's still `App`'s job (ratatui has no
    // concept of "page size" to ask a `Table` for).
    app.raw_visible_rows = table_area.height.saturating_sub(1).max(1) as usize;
    // For `App::raw_row_at` to map a click back to a feature index later.
    app.raw_table_area = table_area;

    let table = build_table(caps, &app.raw_readings);
    app.raw_table_state.select(Some(app.raw_cursor));
    frame.render_stateful_widget(table, table_area, &mut app.raw_table_state);

    if caps.features.len() > app.raw_visible_rows {
        let mut sb_state = ScrollbarState::new(caps.features.len())
            .position(app.raw_cursor)
            .viewport_content_length(app.raw_visible_rows);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        frame.render_stateful_widget(scrollbar, table_area, &mut sb_state);
    }

    let status = if app.raw_writing {
        Line::styled("Writing...", styles::dim())
    } else if let Some(e) = &app.raw_write_err {
        Line::styled(format!("Write failed: {e}"), styles::err())
    } else {
        Line::raw("")
    };
    frame.render_widget(status, status_area);

    frame.render_widget(
        Line::styled(
            "↑↓/j/k move · click/scroll row · f/pgdn b/pgup page · e edit value · r rescan · esc/v back · q quit",
            styles::dim(),
        ),
        help_area,
    );
}

/// Builds the feature table widget from data alone (no `App`), so the
/// focused-row test below can render it into an off-screen buffer without
/// a real terminal.
fn build_table<'a>(caps: &Capabilities, readings: &HashMap<u8, FeatureReading>) -> Table<'a> {
    let header = Row::new(["Code", "Name", "Category", "Value"]).style(styles::dim());

    let rows = caps.features.iter().map(|f| {
        let mut name = f.name.clone();
        if name.chars().count() > 34 {
            name = name.chars().take(31).collect::<String>() + "...";
        }

        let (category, cat_style) = if !f.recognized {
            if f.manufacturer_specific {
                ("mfg-specific", styles::dim())
            } else {
                ("unknown", styles::dim())
            }
        } else {
            ("known", styles::ok())
        };

        Row::new([
            Cell::from(format!("{:02X}", f.code)),
            Cell::from(name),
            Cell::from(category).style(cat_style),
            Cell::from(raw_value_string(readings.get(&f.code))),
        ])
    });

    Table::new(rows, WIDTHS)
        .header(header)
        .row_highlight_style(styles::raw_focused())
        .highlight_symbol("▸ ")
}

fn raw_value_string(reading: Option<&FeatureReading>) -> String {
    let Some(r) = reading else {
        return "(not probed)".to_string();
    };
    if !r.readable {
        return "write-only (action)".to_string();
    }
    if r.continuous {
        return format!("{} / {}", r.current, r.max);
    }
    if let Some(raw) = &r.raw {
        return format!(
            "0x{:02X}  (mh={:02X} ml={:02X} sh={:02X} sl={:02X})",
            r.current, raw.mh, raw.ml, raw.sh, raw.sl
        );
    }
    if r.generic {
        // No value code was ever parsed for these (VCP version, frequency,
        // firmware level, ...) — `current` is just its default, not a real
        // reading, so showing it alongside the label would be a fabricated
        // fact.
        return r.label.clone();
    }
    if !r.label.is_empty() {
        return format!("{} (0x{:02X})", r.label, r.current);
    }
    format!("0x{:02X}", r.current)
}

/// The numeric-entry prompt for the currently selected row.
fn render_edit(app: &App) -> Vec<Line<'static>> {
    let Some(caps) = &app.caps else { return vec![] };
    let f = &caps.features[app.raw_cursor];

    let mut lines = vec![Line::from(format!("Set {:02X} ({})", f.code, f.name))];
    if !f.recognized {
        lines.push(Line::styled(
            "(unrecognized/manufacturer-specific — write needs permit-unknown)",
            styles::dim(),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(format!("New value: {}█", app.raw_edit_input)));

    if !app.raw_edit_err.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(app.raw_edit_err.clone(), styles::err()));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "0-9 enter digits · enter confirm · backspace delete · esc cancel",
        styles::dim(),
    ));
    lines
}

/// The y/N gate shown after a valid value is entered, before it's actually
/// sent to the monitor.
fn render_confirm(app: &App) -> Vec<Line<'static>> {
    let Some(caps) = &app.caps else { return vec![] };
    let f = &caps.features[app.raw_cursor];

    let mut lines = vec![
        Line::styled("⚠ Write raw VCP value", styles::err()),
        Line::raw(""),
        Line::raw(format!(
            "This writes {} (0x{:02X}) to code {:02X} ({}) directly.",
            app.raw_confirm_value, app.raw_confirm_value, f.code, f.name
        )),
    ];
    if !f.recognized {
        lines.push(Line::raw(
            "This code is not recognized — sending an arbitrary value to it",
        ));
        lines.push(Line::raw(
            "is undocumented behavior; it may do nothing, or it may affect",
        ));
        lines.push(Line::raw("the monitor in a way this tool can't warn about."));
    }
    lines.push(Line::raw("There is no undo from software once it's sent."));
    lines.push(Line::raw(""));
    lines.push(Line::raw("Proceed? [y/N]"));
    lines.push(Line::raw(""));
    lines.push(Line::styled("y confirm · n/esc cancel", styles::dim()));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcp::{RawBytes, VcpFeature};
    use ratatui::buffer::Buffer;
    use ratatui::widgets::{StatefulWidget, TableState};

    fn feature(code: u8, name: &str) -> VcpFeature {
        VcpFeature {
            code,
            name: name.to_string(),
            recognized: true,
            manufacturer_specific: false,
            values: Vec::new(),
        }
    }

    #[test]
    fn raw_value_string_generic_reading_omits_fabricated_code() {
        let r = FeatureReading {
            readable: true,
            generic: true,
            label: "1164 hz".to_string(),
            ..Default::default()
        };
        let got = raw_value_string(Some(&r));
        assert_eq!(got, "1164 hz", "must not fabricate a hex code for a generic reading");
        assert!(!got.contains("0x00"));
    }

    #[test]
    fn raw_value_string_known_enum_keeps_code() {
        let r = FeatureReading {
            readable: true,
            label: "6500 K".to_string(),
            current: 0x05,
            ..Default::default()
        };
        assert_eq!(raw_value_string(Some(&r)), "6500 K (0x05)");
    }

    #[test]
    fn raw_value_string_not_probed() {
        let got = raw_value_string(None);
        assert!(got.contains("not probed"), "got {got:?}");
    }

    #[test]
    fn raw_value_string_not_readable_action() {
        let r = FeatureReading {
            readable: false,
            ..Default::default()
        };
        let got = raw_value_string(Some(&r));
        assert!(got.contains("write-only"), "got {got:?}");
    }

    #[test]
    fn raw_value_string_raw_unknown_shows_bytes() {
        let r = FeatureReading {
            readable: true,
            current: 0x1f,
            raw: Some(RawBytes { mh: 0xff, ml: 0xff, sh: 0x00, sl: 0x1f }),
            ..Default::default()
        };
        let got = raw_value_string(Some(&r));
        assert!(got.contains("sl=1F") || got.contains("sl=1f"), "got {got:?}");
    }

    /// Renders `build_table`'s output into an off-screen buffer (the
    /// standard way to unit-test a ratatui widget without a real
    /// terminal) and reads back which row actually carries the highlight
    /// symbol — the Rust equivalent of the Go original's string-based
    /// `renderRawTable` cursor-marker test, just operating on rendered
    /// cells instead of a formatted string.
    #[test]
    fn table_marks_focused_row() {
        let caps = Capabilities {
            model: String::new(),
            mccs_version: String::new(),
            features: vec![feature(0x10, "Brightness"), feature(0x12, "Contrast")],
        };

        let area = Rect::new(0, 0, 90, 4); // header + 2 data rows + slack
        let mut buf = Buffer::empty(area);
        let table = build_table(&caps, &HashMap::new());
        let mut state = TableState::default().with_selected(Some(1));
        StatefulWidget::render(table, area, &mut buf, &mut state);

        let row_text = |y: u16| -> String {
            (area.x..area.x + area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect()
        };

        // Row 0 of the buffer is the table's own header; data rows follow.
        assert!(!row_text(1).contains('▸'), "row 0 (unfocused) should have no cursor marker");
        assert!(row_text(2).contains('▸'), "row 1 (focused) should have the cursor marker");
    }
}
