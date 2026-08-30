//! Top-level render dispatch. Text-flowing screens (Controls, the Picker,
//! and the Raw VCP screen's own edit/confirm/loading/error sub-states)
//! build their content as `Vec<Line>` (mirroring the Go original's
//! string-building `View()` methods) and get wrapped in the shared
//! rounded-border box by `render_box`. The Raw VCP screen's scrollable
//! table is the one exception — it needs a real `Rect` to render a
//! stateful `Table` widget into, so it draws itself (see `screens::raw`).

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::app::{App, ClickTarget, Screen};
use crate::screens;
use crate::styles;

pub const TITLE: &str = "CMMRS";

/// Rows `render_box` always reserves around/above the scrollable body it's
/// handed: the top and bottom border (2), the block's top and bottom
/// padding (2, see `Padding::new(4, 4, 1, 1)` below), the title line, and
/// the blank line under it. Fixed regardless of screen content — used both
/// to decide how tall the box needs to be and, on the Controls/Picker
/// side, to keep `App::click_origin_row` in sync with wherever the body
/// actually ended up landing.
const BOX_CHROME_ROWS: u16 = 6;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();

    match app.screen {
        Screen::Picker => {
            let (lines, targets) = screens::picker::render(app);
            let focus = targets.iter().position(|t| *t == Some(ClickTarget::Display(app.picker_cursor)));
            draw_scrollable(frame, area, app, lines, targets, focus);
        }
        Screen::Raw => screens::raw::draw(frame, area, app),
        Screen::Controls => {
            if app.confirming {
                // Keyboard-only prompt (see `App::handle_mouse`'s docs) —
                // short and fixed, so it always fits; no scrolling, no
                // click targets.
                app.click_targets.clear();
                app.click_scroll = 0;
                let lines = screens::controls::render_confirm(app);
                render_box(frame, area, TITLE, lines, 0);
            } else {
                let (lines, targets) = screens::controls::render(app);
                let focus = targets.iter().position(|t| *t == Some(ClickTarget::Order(app.cursor)));
                draw_scrollable(frame, area, app, lines, targets, focus);
            }
        }
    }
}

/// Shared by Controls and Picker: renders `lines`, keeping `focus` (an
/// index into `lines`, if any) scrolled into view, then stashes the mouse
/// hit-testing state (`click_targets`/`click_origin_row`/`click_scroll`)
/// `App::handle_mouse` needs on the next input event.
fn draw_scrollable(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    lines: Vec<Line<'static>>,
    targets: Vec<Option<ClickTarget>>,
    focus: Option<usize>,
) {
    let visible_rows = area.height.saturating_sub(BOX_CHROME_ROWS);
    app.click_scroll = ensure_visible(app.click_scroll, lines.len(), visible_rows, focus);
    let body_area = render_box(frame, area, TITLE, lines, app.click_scroll);
    app.click_targets = targets;
    app.click_origin_row = body_area.y;
    app.click_origin_col = body_area.x;
}

/// The smallest scroll offset (starting from `scroll`, previous frame's
/// value) that both stays in range for `total_lines` shown across
/// `visible_rows` and keeps `focus` on screen — nudges up or down just
/// enough to bring it back into view, same as any list widget's "keep the
/// selection visible" behavior, rather than re-centering on every move.
fn ensure_visible(scroll: u16, total_lines: usize, visible_rows: u16, focus: Option<usize>) -> u16 {
    let max_scroll = (total_lines as u16).saturating_sub(visible_rows);
    let mut scroll = scroll.min(max_scroll);
    if let Some(focus) = focus {
        let focus = focus as u16;
        if focus < scroll {
            scroll = focus;
        } else if visible_rows > 0 && focus >= scroll + visible_rows {
            scroll = focus + 1 - visible_rows;
        }
    }
    scroll
}

/// Wraps `lines` in the shared rounded box with a centered, fixed title
/// line above them — same look as the Go original's `boxStyle` (rounded
/// border, padding 1/4) applied around `title + "\n\n" + body`. Shared
/// with `screens::raw` for its non-table sub-states.
///
/// The box shrinks to fit `lines` rather than always stretching to fill
/// `area` — a short confirmation prompt no longer stretches into a mostly
/// empty full-terminal box — growing to `area`'s full height only once
/// content doesn't fit, at which point `scroll` (rows of `lines` to skip;
/// 0 for every caller that never overflows) keeps it from spilling past
/// the bottom border instead of just clipping mid-frame. Returns the body
/// sub-area actually drawn into — callers that support mouse use it for
/// `App::click_origin_row`; everyone else just ignores it.
pub(crate) fn render_box(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    scroll: u16,
) -> Rect {
    let height = (lines.len() as u16).saturating_add(BOX_CHROME_ROWS).min(area.height);
    let box_area = Rect { height, ..area };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(ratatui::style::Style::default().fg(styles::BORDER_COLOR))
        .padding(Padding::new(4, 4, 1, 1));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    // Title pinned at the top, body below it scrolls on its own — folding
    // the title into the same scrollable `Paragraph` as the body (the
    // previous approach) would scroll it away too.
    let [title_area, _, body_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    frame.render_widget(
        Line::styled(title, styles::title()).alignment(Alignment::Center),
        title_area,
    );

    // No `.wrap(..)`: `scroll`, `click_targets`, and `ensure_visible` all
    // assume one `Line` renders as exactly one row — a wrapped line would
    // silently break that (everything after it scrolls/hit-tests off by
    // however many extra rows it took), trading a correct click/scroll
    // mapping for reflow that was cosmetic at best. A line wider than the
    // box just gets truncated at the border instead, same trade-off most
    // row-addressable TUI lists make (a table can't wrap a row either).
    let total_lines = lines.len();
    let paragraph = Paragraph::new(Text::from(lines)).scroll((scroll, 0));
    frame.render_widget(paragraph, body_area);

    // Same visual cue `screens::raw`'s table gives for "there's more than
    // fits" — only worth drawing once content actually overflows, same
    // condition `ensure_visible` uses to decide whether `scroll` can be
    // nonzero at all.
    if total_lines as u16 > body_area.height {
        let mut sb_state = ScrollbarState::new(total_lines)
            .position(scroll as usize)
            .viewport_content_length(body_area.height as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        frame.render_stateful_widget(scrollbar, body_area, &mut sb_state);
    }

    body_area
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ClickTarget;
    use crate::commands::{CtrlKind, CtrlRef};
    use crate::components::Slider;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// A Controls screen with `n` sliders — enough to reproduce "way more
    /// controls than a short terminal has rows for" (see the bug report
    /// this fixed: a tiny embedded terminal pane, content spilling past
    /// where the box's bottom border should have closed).
    fn app_with_n_sliders(n: usize) -> App {
        let mut app = App::new();
        app.loading = false;
        // `body_lines` bails out to "No DDC/CI capable displays found."
        // before ever looking at `caps`/`order` unless there's at least
        // one display.
        app.displays = vec![crate::vcp::Display {
            number: 1,
            mfg_id: "Test".to_string(),
            ..Default::default()
        }];
        // `body_lines` only renders `order` once `caps` is `Some` — the
        // features inside don't otherwise need to line up with the
        // sliders for this test, just be present.
        app.caps = Some(crate::vcp::Capabilities {
            model: String::new(),
            mccs_version: "2.1".to_string(),
            features: (0..n)
                .map(|i| crate::vcp::VcpFeature {
                    code: i as u8,
                    name: format!("Slider {i}"),
                    recognized: true,
                    manufacturer_specific: false,
                    values: Vec::new(),
                })
                .collect(),
        });
        app.sliders = (0..n).map(|i| Slider::new(i as u8, format!("Slider {i}"), 50, 100)).collect();
        app.order = (0..n)
            .map(|i| CtrlRef {
                kind: CtrlKind::Slider,
                idx: i,
            })
            .collect();
        app
    }

    /// Renders `app` into a `rows`-tall buffer and returns each row's text
    /// (trimmed of trailing spaces) — enough to assert on without pulling
    /// in styling.
    fn render_rows(app: &mut App, width: u16, rows: u16) -> Vec<String> {
        let backend = TestBackend::new(width, rows);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..rows)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .map(|s| s.trim_end().to_string())
            .collect()
    }

    #[test]
    fn short_content_does_not_stretch_the_box_to_fill_a_tall_terminal() {
        let mut app = app_with_n_sliders(2);
        let rows = render_rows(&mut app, 60, 40);

        // Content: 2 display-ish/header lines' worth is skipped here since
        // there's no display — just assert the box's bottom border shows
        // up well before the bottom of a 40-row terminal, i.e. it didn't
        // stretch to fill it.
        let border_row = rows.iter().position(|r| r.contains('╰')).expect("expected a bottom border somewhere");
        assert!(border_row < 20, "box with 2 sliders stretched to fill a 40-row terminal (border at row {border_row})");
    }

    #[test]
    fn tall_content_scrolls_instead_of_overflowing_a_short_terminal() {
        // Comfortably more sliders than 10 rows has room for.
        let mut app = app_with_n_sliders(20);
        let rows = render_rows(&mut app, 60, 10);

        // The box must still close within the given area — no content (or
        // a missing border) past the last row.
        let last = rows.last().unwrap();
        assert!(
            last.contains('╰'),
            "expected the bottom border on the last row of a 10-row terminal, got {last:?}"
        );
        // And the box must open at the very top — it had nowhere else to
        // put 20 sliders' worth of content, so it should claim the whole
        // area, not stop short and lose rows to nothing.
        assert!(rows[0].contains('╭'), "expected the top border on row 0, got {:?}", rows[0]);
    }

    #[test]
    fn scrolling_keeps_the_focused_control_visible_and_click_targets_in_sync() {
        let mut app = app_with_n_sliders(20);
        app.cursor = 15; // focused control well past what 10 rows can show from the top
        let rows = render_rows(&mut app, 60, 10);

        assert!(
            rows.iter().any(|r| r.contains("Slider 15")),
            "the focused control must have been scrolled into view"
        );
        // click_targets/click_scroll must agree with what's actually on
        // screen: clicking the row showing "Slider 15" should resolve to
        // its real Order index (15), not whatever index would've been
        // there with no scrolling.
        let screen_row = rows.iter().position(|r| r.contains("Slider 15")).unwrap() as u16;
        let target = app.click_targets[app.click_scroll as usize + (screen_row - app.click_origin_row) as usize];
        assert_eq!(target, Some(ClickTarget::Order(15)));
    }
}


