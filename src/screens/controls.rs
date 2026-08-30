//! The main controls screen — port of the bulk of the Go original's
//! `model.go` `View()`.

use ratatui::text::{Line, Span};

use crate::app::App;
use crate::commands::CtrlKind;
use crate::styles;

pub fn render(app: &App) -> Vec<Line<'static>> {
    let mut lines = body_lines(app);

    let mut help = "↑↓ navigate · ←→ adjust · enter run action · v raw VCP · r refresh · R rescan".to_string();
    if app.displays.len() > 1 {
        help.push_str(" · D switch display");
    }
    help.push_str(" · q quit");

    lines.push(Line::raw(""));
    lines.push(Line::styled(help, styles::dim()));
    lines
}

fn body_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if app.loading {
        lines.push(Line::styled("Detecting monitors...", styles::dim()));
        return lines;
    }
    if let Some(e) = &app.err {
        lines.push(Line::styled(format!("Error: {e}"), styles::err()));
        return lines;
    }
    if app.displays.is_empty() {
        lines.push(Line::styled(
            "No DDC/CI capable displays found.",
            styles::err(),
        ));
        return lines;
    }

    let multi = app.displays.len() > 1;
    for (i, d) in app.displays.iter().enumerate() {
        let prefix = if multi {
            if i == app.selected {
                "▸ "
            } else {
                "  "
            }
        } else {
            ""
        };
        let mut spans = vec![Span::raw(prefix), Span::styled("● ", styles::ok())];
        spans.push(Span::raw(d.mfg_id.clone()));
        if !d.model.is_empty() {
            spans.push(Span::raw(format!(" {}", d.model)));
        }
        spans.push(Span::styled(
            format!(" ({}, VCP {})", d.bus, d.vcp_version),
            styles::dim(),
        ));
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));

    if app.probing {
        lines.push(Line::styled("Reading VCP features...", styles::dim()));
        return lines;
    }
    if let Some(e) = &app.probe_err {
        lines.push(Line::styled(format!("Probe error: {e}"), styles::err()));
        return lines;
    }
    let Some(caps) = &app.caps else {
        return lines;
    };

    let unknown = caps.features.iter().filter(|f| !f.recognized).count();
    let shown = app.sliders.len() + app.selectors.len() + app.actions.len();

    lines.push(Line::from(vec![
        Span::styled("● ", styles::ok()),
        Span::raw(format!("MCCS {}", caps.mccs_version)),
    ]));
    lines.push(Line::styled(
        summary_line(caps.features.len(), shown, unknown),
        styles::dim(),
    ));
    lines.push(Line::raw(""));

    for (i, r) in app.order.iter().enumerate() {
        let focused = i == app.cursor;
        let (mut line, code) = match r.kind {
            CtrlKind::Slider => {
                let s = &app.sliders[r.idx];
                (s.view(focused), s.code)
            }
            CtrlKind::Selector => {
                let s = &app.selectors[r.idx];
                (s.view(focused), s.code)
            }
            CtrlKind::Action => {
                let a = &app.actions[r.idx];
                (a.view(focused), a.code)
            }
        };
        if app.pending.contains(&code) {
            line.spans.push(Span::styled(" …", styles::dim()));
        }
        lines.push(line);
    }

    if let Some(e) = &app.op_err {
        lines.push(Line::raw(""));
        lines.push(Line::styled(format!("Failed: {e}"), styles::err()));
    }

    lines
}

fn summary_line(total: usize, shown: usize, unknown: usize) -> String {
    format!(
        "{total} VCP features declared · {shown} shown as controls · {unknown} unrecognized/mfg-specific"
    )
}

/// The y/N gate shown before a destructive action (e.g. "Restore factory
/// defaults") is actually sent.
pub fn render_confirm(app: &App) -> Vec<Line<'static>> {
    let a = &app.actions[app.confirm_action_idx];
    vec![
        Line::styled(format!("⚠ {}", a.name), styles::err()),
        Line::raw(""),
        Line::raw(format!(
            "This writes \"{}\" to the monitor directly.",
            a.name
        )),
        Line::raw("There is no undo from software once it's sent."),
        Line::raw(""),
        Line::raw("Proceed? [y/N]"),
        Line::raw(""),
        Line::styled("y confirm · n/esc cancel", styles::dim()),
    ]
}
