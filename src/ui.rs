//! Top-level render dispatch. Text-flowing screens (Controls, the Picker,
//! and the Raw VCP screen's own edit/confirm/loading/error sub-states)
//! build their content as `Vec<Line>` (mirroring the Go original's
//! string-building `View()` methods) and get wrapped in the shared
//! rounded-border box by `render_box`. The Raw VCP screen's scrollable
//! table is the one exception — it needs a real `Rect` to render a
//! stateful `Table` widget into, so it draws itself (see `screens::raw`).

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Screen};
use crate::screens;
use crate::styles;

pub const TITLE: &str = "CMMRS";

/// Rows `render_box` always reserves above the body it's handed: the top
/// border (1), the block's top padding (1, see `Padding::new(4, 4, 1, 1)`
/// below), the title line, and the blank line under it. Fixed regardless
/// of screen content, so `App::click_origin_row` is just `area.y +` this,
/// on every screen `render_box` wraps.
const BOX_HEADER_ROWS: u16 = 4;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();

    match app.screen {
        Screen::Picker => {
            let (lines, targets) = screens::picker::render(app);
            app.click_targets = targets;
            app.click_origin_row = area.y + BOX_HEADER_ROWS;
            render_box(frame, area, TITLE, lines);
        }
        Screen::Raw => screens::raw::draw(frame, area, app),
        Screen::Controls => {
            let lines = if app.confirming {
                app.click_targets.clear(); // keyboard-only prompt, see App::handle_mouse
                screens::controls::render_confirm(app)
            } else {
                let (lines, targets) = screens::controls::render(app);
                app.click_targets = targets;
                app.click_origin_row = area.y + BOX_HEADER_ROWS;
                lines
            };
            render_box(frame, area, TITLE, lines);
        }
    }
}

/// Wraps `lines` in the shared rounded box with a centered title line above
/// them, same look as the Go original's `boxStyle` (rounded border,
/// padding 1/4) applied around `title + "\n\n" + body`. Shared with
/// `screens::raw` for its non-table sub-states.
pub(crate) fn render_box(frame: &mut Frame<'_>, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(ratatui::style::Style::default().fg(styles::BORDER_COLOR))
        .padding(Padding::new(4, 4, 1, 1));

    let mut all = Vec::with_capacity(lines.len() + 2);
    all.push(Line::styled(title, styles::title()).alignment(ratatui::layout::Alignment::Center));
    all.push(Line::raw(""));
    all.extend(lines);

    let paragraph = Paragraph::new(ratatui::text::Text::from(all))
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}
