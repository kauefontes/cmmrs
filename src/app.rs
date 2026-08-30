//! Application state and its update logic — the Rust analogue of the Go
//! original's `tui.Model` (`model.go` + `rawview.go` combined).
//!
//! `App` never touches a `DdcBackend` or a channel: every method that
//! needs I/O just *describes* it by returning a `commands::Cmd`, which
//! `main`'s event loop is responsible for actually dispatching. That split
//! is what makes this testable the same way the Go original's tests drive
//! `Model.Update` directly with synthetic messages and inspect the
//! returned `tea.Cmd` without ever running a real subprocess — see the
//! `tests` module at the bottom of this file.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use tachyonfx::{fx, EffectManager, Interpolation};

use crate::cache;
use crate::commands::{self, Cmd, CtrlKind, CtrlRef, Msg};
use crate::components::{Action, Selector, Slider};
use crate::styles;
use crate::vcp::{Capabilities, Display, FeatureReading};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    Controls,
    Raw,
    Picker,
}

/// What clicking a given line means, on whichever text screen (Controls or
/// Picker — the two `ui::render_box`-wrapped, `Vec<Line>`-flowing screens)
/// most recently rendered it. `App::click_targets` holds one of these (or
/// `None`, for a blank/header line) per line, rebuilt every frame; see its
/// docs for how a screen row maps back to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickTarget {
    /// A display row — one of the header lines on Controls (only
    /// meaningful with more than one display), or a row on the Picker.
    Display(usize),
    /// A control row on Controls, indexing `App::order`.
    Order(usize),
}

#[derive(Default)]
pub struct App {
    pub should_quit: bool,

    pub loading: bool,
    pub displays: Vec<Display>,
    pub err: Option<String>,

    /// Indexes `displays` for whichever one is currently being controlled.
    /// `display_chosen` stays false until either a single display was
    /// auto-picked or the picker screen (for more than one) has been
    /// answered — it's what keeps a later refresh from re-showing the
    /// picker and bouncing the user back to display 0.
    pub selected: usize,
    pub display_chosen: bool,
    pub picker_cursor: usize,

    pub probing: bool,
    pub caps: Option<Capabilities>,
    pub probe_err: Option<String>,

    pub sliders: Vec<Slider>,
    pub selectors: Vec<Selector>,
    pub actions: Vec<Action>,
    pub order: Vec<CtrlRef>,
    pub cursor: usize,
    pub op_err: Option<String>,

    /// Controls whose value came from cache and hasn't been confirmed by a
    /// live read yet. Empty after a fresh (non-cached) scan, since there's
    /// nothing left to confirm by then.
    pub pending: HashSet<u8>,

    /// Set while a destructive action is awaiting a y/n answer. Every
    /// other key is swallowed until it's answered.
    pub confirming: bool,
    pub confirm_action_idx: usize,

    pub screen: Screen,
    pub raw_ready: bool,
    pub raw_loading: bool,
    pub raw_err: Option<String>,
    pub raw_readings: HashMap<u8, FeatureReading>,
    /// Index into `caps.features` of the focused row.
    pub raw_cursor: usize,
    /// How many data rows the table area last had room for — `page_raw`
    /// uses this to size a page-jump. Set each time the screen is drawn
    /// (`screens::raw::draw_table`).
    pub raw_visible_rows: usize,
    /// `ratatui::widgets::Table`'s scroll position/selection for the Raw
    /// VCP screen. Owned here (not recomputed fresh each frame) so the
    /// scroll offset persists smoothly across renders — the actual
    /// "keep the selected row visible" logic is ratatui's, not
    /// hand-rolled; `App` only ever moves `raw_cursor` and syncs it into
    /// this state's selection right before rendering.
    pub raw_table_state: TableState,

    pub raw_editing: bool,
    pub raw_edit_input: String,
    pub raw_edit_err: String,

    /// Gates the actual write behind a y/n prompt — writing an arbitrary
    /// value to an unrecognized/manufacturer-specific code is undocumented
    /// backend behavior, so this never fires silently.
    pub raw_confirming: bool,
    pub raw_confirm_value: u16,
    pub raw_writing: bool,
    pub raw_write_err: Option<String>,

    // ---- mouse hit-testing ---------------------------------------------
    //
    // Rendering is a pure `App -> Vec<Line>` function everywhere except
    // here: these are the one bit of render output `ui`/`screens` feed
    // back into `App`, since hit-testing a click needs to know where
    // things ended up on screen and nothing else already tracks that.
    // Rebuilt on every frame `ui::draw` renders (see `screens::controls`/
    // `screens::picker`), read by `handle_mouse` on the next input event —
    // one frame stale at worst, same lag any GUI has between a redraw and
    // the next click.
    /// Line-index → click target for whichever `ui::render_box`-wrapped
    /// screen (Controls or Picker) was last drawn; `None` for a line
    /// that's blank, a header, or otherwise unclickable. Index 0 is the
    /// *first line of content*, not necessarily what's on screen at
    /// `click_origin_row` — see `click_scroll`.
    pub click_targets: Vec<Option<ClickTarget>>,
    /// Terminal-absolute row of the body's first *visible* line as of the
    /// last render (`render_box`'s returned body `Rect`'s `y`).
    pub click_origin_row: u16,
    /// Terminal-absolute column where each rendered line starts
    /// (`render_box`'s returned body `Rect`'s `x`) — `target_at` only
    /// needs the row to find *which* control a click landed on, but a
    /// slider's bar needs the column too, to know *where in it*.
    pub click_origin_col: u16,
    /// How many lines of `click_targets` are scrolled off the top when
    /// content doesn't fit in the available height — `click_origin_row`
    /// plus this many is where `click_targets[0]` would be if it weren't
    /// off screen. 0 whenever everything fits, which is the common case.
    pub click_scroll: u16,
    /// The Raw VCP screen's table `Rect` as of the last render — hit-
    /// testing a click there also needs `raw_table_state`'s scroll offset
    /// (already `App`'s), so this is the only extra geometry it needs.
    pub raw_table_area: Rect,

    /// Shader-like frame effects (entrance/transition fades, an error
    /// flash — see `trigger_entrance_fx`/`trigger_transition_fx`/
    /// `trigger_error_fx`) — cross-frame render state in the same vein as
    /// `raw_table_state`/`raw_table_area` above, so it lives here for the
    /// same reason those do. `main.rs` drains it every frame via
    /// `process_effects`; nothing about *what* effect runs when depends
    /// on rendering, only *that* one was queued, which is decided here
    /// alongside the state change that motivated it.
    pub effects: EffectManager<()>,
}

impl App {
    pub fn new() -> Self {
        App {
            loading: true,
            raw_visible_rows: 10,
            ..Default::default()
        }
    }

    /// The initial command to kick off discovery — call once after
    /// construction, e.g. `commands::dispatch(App::init(), &backend, &tx)`.
    pub fn init() -> Cmd {
        Cmd::Detect
    }

    pub fn current_display(&self) -> Option<&Display> {
        if self.displays.is_empty() {
            return None;
        }
        let idx = if self.selected < self.displays.len() {
            self.selected
        } else {
            0
        };
        self.displays.get(idx)
    }

    pub fn display_num(&self) -> i32 {
        self.current_display().map(|d| d.number).unwrap_or(0)
    }

    // ---- effects --------------------------------------------------------
    //
    // Kept short and "subtle and fast" on purpose (fixed ~200-300ms, one
    // easing curve) rather than exposed as configurable — these are a
    // finishing touch, not a feature surface.

    /// The Controls screen just got real content for the first time this
    /// scan (a fresh probe landed, or a cached one did) — fades in from
    /// the border color rather than popping onto screen instantly.
    fn trigger_entrance_fx(&mut self) {
        self.effects.add_effect(fx::fade_from(
            styles::BORDER_COLOR,
            styles::BORDER_COLOR,
            (250, Interpolation::QuadOut),
        ));
    }

    /// Switched between two already-populated screens (Controls/Raw/
    /// Picker) — same fade, just shorter, since there's no "first load"
    /// wait backing it up.
    fn trigger_transition_fx(&mut self) {
        self.effects.add_effect(fx::fade_from(
            styles::BORDER_COLOR,
            styles::BORDER_COLOR,
            (150, Interpolation::QuadOut),
        ));
    }

    /// A new error just appeared (not still-being-shown — call this only
    /// from the branch that assigns it, so it fires once per failure, not
    /// once per render).
    fn trigger_error_fx(&mut self) {
        self.effects.add_effect(fx::fade_from(styles::ERR_COLOR, styles::ERR_COLOR, (200, Interpolation::QuadOut)));
    }

    /// Commits to controlling `displays[idx]` and (re)starts the probe for
    /// it. Used both for the very first pick and for switching displays
    /// later via `D` — either way, every bit of state from whatever was
    /// previously being controlled has to be dropped, not just left to be
    /// silently overwritten by the new probe.
    fn select_display(&mut self, idx: usize) -> Option<Cmd> {
        let display = self.displays.get(idx).cloned()?;

        self.selected = idx;
        self.display_chosen = true;
        self.screen = Screen::Controls;
        self.probing = true;
        self.caps = None;
        self.probe_err = None;
        self.sliders.clear();
        self.selectors.clear();
        self.actions.clear();
        self.order.clear();
        self.cursor = 0;
        self.op_err = None;
        self.pending.clear();
        self.confirming = false;
        self.reset_raw();

        Some(Cmd::Probe(display))
    }

    fn reset_raw(&mut self) {
        self.raw_ready = false;
        self.raw_loading = false;
        self.raw_err = None;
        self.raw_readings.clear();
        self.raw_cursor = 0;
        self.raw_table_state = TableState::default();
        self.raw_editing = false;
        self.raw_edit_input.clear();
        self.raw_edit_err.clear();
        self.raw_confirming = false;
        self.raw_writing = false;
        self.raw_write_err = None;
    }

    fn refresh(&mut self) -> Cmd {
        self.loading = true;
        self.err = None;
        self.probing = false;
        self.caps = None;
        self.probe_err = None;
        self.sliders.clear();
        self.selectors.clear();
        self.actions.clear();
        self.order.clear();
        self.cursor = 0;
        self.op_err = None;
        self.pending.clear();
        self.confirming = false;
        self.screen = Screen::Controls;
        self.reset_raw();
        Cmd::Detect
    }

    /// Moves the focused slider/selector one step in the given direction
    /// (-1 or +1). No-op if there's nothing to adjust, the focused control
    /// is an action, or it's already at that end (a slider at its bound, a
    /// single-option selector). This never mutates a control's displayed
    /// value directly — that only happens once `Msg::Set` confirms the
    /// write actually took effect.
    fn adjust(&mut self, direction: i32) -> Option<Cmd> {
        let ref_ = *self.order.get(self.cursor)?;
        match ref_.kind {
            CtrlKind::Slider => {
                let s = &self.sliders[ref_.idx];
                let step = s.step as i32 * direction;
                let new_value = (s.value as i32 + step).clamp(0, s.max as i32) as u16;
                if new_value == s.value {
                    return None;
                }
                self.op_err = None;
                Some(Cmd::Set {
                    display_num: self.display_num(),
                    code: s.code,
                    value: new_value,
                })
            }
            CtrlKind::Selector => {
                let sel = &self.selectors[ref_.idx];
                let next_code = sel.next_option(direction);
                if next_code == sel.selected {
                    return None;
                }
                self.op_err = None;
                Some(Cmd::Set {
                    display_num: self.display_num(),
                    code: sel.code,
                    value: next_code as u16,
                })
            }
            CtrlKind::Action => None,
        }
    }

    // ---- key handling ------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Cmd> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return None;
        }

        if self.confirming {
            return self.handle_key_confirming(key);
        }
        if self.raw_confirming {
            return self.handle_key_raw_confirming(key);
        }
        if self.raw_editing {
            return self.handle_key_raw_editing(key);
        }
        match self.screen {
            Screen::Picker => self.handle_key_picker(key),
            Screen::Raw => self.handle_key_raw(key),
            Screen::Controls => self.handle_key_controls(key),
        }
    }

    fn handle_key_confirming(&mut self, key: KeyEvent) -> Option<Cmd> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let a = self.actions[self.confirm_action_idx].clone();
                self.confirming = false;
                self.op_err = None;
                Some(Cmd::Action {
                    display_num: self.display_num(),
                    code: a.code,
                })
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.confirming = false;
                None
            }
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            _ => None,
        }
    }

    fn handle_key_raw_confirming(&mut self, key: KeyEvent) -> Option<Cmd> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let caps = self.caps.as_ref()?;
                let f = &caps.features[self.raw_cursor];
                let code = f.code;
                let permit_unknown = !f.recognized;
                let value = self.raw_confirm_value;
                self.raw_confirming = false;
                self.raw_writing = true;
                self.raw_write_err = None;
                Some(Cmd::RawSet {
                    display_num: self.display_num(),
                    code,
                    value,
                    permit_unknown,
                })
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.raw_confirming = false;
                None
            }
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            _ => None,
        }
    }

    fn handle_key_raw_editing(&mut self, key: KeyEvent) -> Option<Cmd> {
        match key.code {
            KeyCode::Esc => {
                self.raw_editing = false;
                self.raw_edit_input.clear();
                self.raw_edit_err.clear();
            }
            KeyCode::Enter => match self.raw_edit_input.parse::<u32>() {
                _ if self.raw_edit_input.is_empty() => {
                    self.raw_edit_err = "Enter a whole number.".to_string();
                }
                Ok(v) if v > 65535 => {
                    self.raw_edit_err = "Value must be between 0 and 65535.".to_string();
                }
                Ok(v) => {
                    self.raw_editing = false;
                    self.raw_edit_err.clear();
                    self.raw_confirming = true;
                    self.raw_confirm_value = v as u16;
                }
                Err(_) => {
                    self.raw_edit_err = "Enter a whole number.".to_string();
                }
            },
            KeyCode::Backspace => {
                self.raw_edit_input.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() && self.raw_edit_input.len() < 5 => {
                self.raw_edit_input.push(c);
                self.raw_edit_err.clear();
            }
            _ => {}
        }
        None // swallow anything else while entering a raw value; never issues a command
    }

    fn handle_key_picker(&mut self, key: KeyEvent) -> Option<Cmd> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            KeyCode::Esc => {
                if self.display_chosen {
                    self.screen = Screen::Controls;
                    self.trigger_transition_fx();
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !self.displays.is_empty() {
                    self.picker_cursor = self
                        .picker_cursor
                        .checked_sub(1)
                        .unwrap_or(self.displays.len() - 1);
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.displays.is_empty() {
                    self.picker_cursor = (self.picker_cursor + 1) % self.displays.len();
                }
                None
            }
            KeyCode::Enter => {
                if self.displays.is_empty() {
                    None
                } else {
                    self.select_display(self.picker_cursor)
                }
            }
            _ => None,
        }
    }

    fn handle_key_raw(&mut self, key: KeyEvent) -> Option<Cmd> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            KeyCode::Esc | KeyCode::Char('v') => {
                self.screen = Screen::Controls;
                self.trigger_transition_fx();
                None
            }
            KeyCode::Char('r') => {
                let caps = self.caps.as_ref()?;
                let codes = commands::all_feature_codes(caps);
                self.raw_loading = true;
                Some(Cmd::RawProbe {
                    display_num: self.display_num(),
                    codes,
                })
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(caps) = &self.caps {
                    if !caps.features.is_empty() {
                        self.raw_cursor = self
                            .raw_cursor
                            .checked_sub(1)
                            .unwrap_or(caps.features.len() - 1);
                    }
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(caps) = &self.caps {
                    if !caps.features.is_empty() {
                        self.raw_cursor = (self.raw_cursor + 1) % caps.features.len();
                    }
                }
                None
            }
            KeyCode::Char('f') | KeyCode::PageDown => {
                self.page_raw(1);
                None
            }
            KeyCode::Char('b') | KeyCode::PageUp => {
                self.page_raw(-1);
                None
            }
            KeyCode::Char('e') => {
                if let Some(caps) = &self.caps {
                    if !caps.features.is_empty() && !self.raw_loading {
                        self.raw_editing = true;
                        self.raw_edit_input.clear();
                        self.raw_edit_err.clear();
                        self.raw_write_err = None;
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn page_raw(&mut self, direction: i32) {
        let Some(caps) = &self.caps else { return };
        let len = caps.features.len();
        if len == 0 {
            return;
        }
        let page = self.raw_visible_rows.max(1) as i32;
        let new_cursor = (self.raw_cursor as i32 + direction * page).clamp(0, len as i32 - 1);
        self.raw_cursor = new_cursor as usize;
    }

    fn handle_key_controls(&mut self, key: KeyEvent) -> Option<Cmd> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            KeyCode::Char('r') => Some(self.refresh()),
            KeyCode::Char('R') => {
                // Full rescan: drop the cached shape and start over as if
                // this were the first time this monitor was seen.
                if let Some(d) = self.current_display() {
                    let _ = cache::clear(&d.mfg_id, &d.model);
                }
                Some(self.refresh())
            }
            KeyCode::Char('D') => {
                if self.displays.len() > 1 {
                    self.screen = Screen::Picker;
                    self.picker_cursor = self.selected;
                    self.trigger_transition_fx();
                }
                None
            }
            KeyCode::Char('v') => {
                self.screen = Screen::Raw;
                self.trigger_transition_fx();
                if !self.raw_ready && !self.raw_loading {
                    if let Some(caps) = &self.caps {
                        let codes = commands::all_feature_codes(caps);
                        self.raw_loading = true;
                        return Some(Cmd::RawProbe {
                            display_num: self.display_num(),
                            codes,
                        });
                    }
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !self.order.is_empty() {
                    self.cursor = self.cursor.checked_sub(1).unwrap_or(self.order.len() - 1);
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.order.is_empty() {
                    self.cursor = (self.cursor + 1) % self.order.len();
                }
                None
            }
            KeyCode::Left | KeyCode::Char('h') => self.adjust(-1),
            KeyCode::Right | KeyCode::Char('l') => self.adjust(1),
            KeyCode::Enter => {
                if let Some(r) = self.order.get(self.cursor) {
                    if r.kind == CtrlKind::Action {
                        self.confirming = true;
                        self.confirm_action_idx = r.idx;
                        self.op_err = None;
                    }
                }
                None
            }
            _ => None,
        }
    }

    // ---- mouse handling --------------------------------------------------

    /// Entry point mirroring `handle_key`'s dispatch, minus the gates that
    /// don't take mouse input at all: `confirming`/`raw_confirming` are
    /// destructive-write and undocumented-code-write prompts respectively
    /// — deliberately keyboard-only (an explicit `y`), so a stray click
    /// can never confirm one — and `raw_editing` is free-text numeric
    /// entry, nothing on screen there to click.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> Option<Cmd> {
        if self.confirming || self.raw_confirming || self.raw_editing {
            return None;
        }
        match self.screen {
            Screen::Picker => self.handle_mouse_picker(ev),
            Screen::Raw => self.handle_mouse_raw(ev),
            Screen::Controls => self.handle_mouse_controls(ev),
        }
    }

    /// The click target under terminal row `row`, per the last render —
    /// see `click_targets`'/`click_origin_row`'/`click_scroll`'s docs.
    fn target_at(&self, row: u16) -> Option<ClickTarget> {
        let visible_idx = row.checked_sub(self.click_origin_row)?;
        let idx = visible_idx as usize + self.click_scroll as usize;
        self.click_targets.get(idx).copied().flatten()
    }

    /// `col`, translated into "column within its own control's rendered
    /// line" — what `Slider::value_at_column` expects. See
    /// `click_origin_col`'s docs.
    fn col_in_line(&self, col: u16) -> Option<u16> {
        col.checked_sub(self.click_origin_col)
    }

    /// Steps a 0-based cursor by one position within `0..len`, wrapping —
    /// the one piece of logic every "move through a list" input shares:
    /// `↑`/`↓` and scroll wheel on both Controls and Picker. `direction >
    /// 0` means "up" (toward index 0), matching `handle_key_controls`'
    /// existing `KeyCode::Up` arm.
    fn step_cursor(cursor: usize, len: usize, direction: i32) -> usize {
        if len == 0 {
            return 0;
        }
        if direction > 0 {
            cursor.checked_sub(1).unwrap_or(len - 1)
        } else {
            (cursor + 1) % len
        }
    }

    /// Sets a slider (identified by its index into `order`) to `value`,
    /// same shape of result `adjust` returns for a step — `None` if it's
    /// not actually a slider, or the value didn't change.
    fn set_slider_value(&mut self, order_idx: usize, value: u16) -> Option<Cmd> {
        let r = *self.order.get(order_idx)?;
        if r.kind != CtrlKind::Slider {
            return None;
        }
        let s = &self.sliders[r.idx];
        if value == s.value {
            return None;
        }
        self.op_err = None;
        Some(Cmd::Set {
            display_num: self.display_num(),
            code: s.code,
            value,
        })
    }

    fn handle_mouse_controls(&mut self, ev: MouseEvent) -> Option<Cmd> {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => match self.target_at(ev.row)? {
                ClickTarget::Display(i) => {
                    if self.displays.len() > 1 && i != self.selected {
                        return self.select_display(i);
                    }
                    None
                }
                ClickTarget::Order(i) => {
                    self.cursor = i;
                    let r = *self.order.get(i)?;
                    match r.kind {
                        // A selector's whole point is a small fixed set of
                        // values — clicking it is naturally "advance to
                        // the next one", same as a click cycles a
                        // dropdown.
                        CtrlKind::Selector => self.adjust(1),
                        CtrlKind::Action => {
                            self.confirming = true;
                            self.confirm_action_idx = r.idx;
                            self.op_err = None;
                            None
                        }
                        // A slider only sets a value when the click
                        // actually landed *inside the bar* — clicking its
                        // name or the "NNN" value text past the bar just
                        // focuses it, same as any other row.
                        CtrlKind::Slider => {
                            let col = self.col_in_line(ev.column)?;
                            let value = self.sliders[r.idx].value_at_column(col)?;
                            self.set_slider_value(i, value)
                        }
                    }
                }
            },
            // Dragging (button still held) across a slider's bar tracks
            // the pointer continuously, same mapping as a click — the
            // rest of a drag (over a selector/action, or off the bar
            // entirely) is a no-op rather than doing anything on every
            // intermediate position.
            MouseEventKind::Drag(MouseButton::Left) => {
                let ClickTarget::Order(i) = self.target_at(ev.row)? else {
                    return None;
                };
                let r = *self.order.get(i)?;
                if r.kind != CtrlKind::Slider {
                    return None;
                }
                let col = self.col_in_line(ev.column)?;
                let value = self.sliders[r.idx].value_at_column(col)?;
                self.set_slider_value(i, value)
            }
            // Scroll always just moves through the list — it never
            // touches a value, regardless of what's under the pointer.
            // (Click/drag on a slider's bar above is the only mouse path
            // that sets one.)
            MouseEventKind::ScrollUp => {
                self.cursor = Self::step_cursor(self.cursor, self.order.len(), 1);
                None
            }
            MouseEventKind::ScrollDown => {
                self.cursor = Self::step_cursor(self.cursor, self.order.len(), -1);
                None
            }
            _ => None,
        }
    }

    fn handle_mouse_picker(&mut self, ev: MouseEvent) -> Option<Cmd> {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => match self.target_at(ev.row)? {
                ClickTarget::Display(i) => {
                    self.picker_cursor = i;
                    self.select_display(i)
                }
                ClickTarget::Order(_) => None, // Picker never emits this
            },
            MouseEventKind::ScrollUp => {
                self.picker_cursor = Self::step_cursor(self.picker_cursor, self.displays.len(), 1);
                None
            }
            MouseEventKind::ScrollDown => {
                self.picker_cursor = Self::step_cursor(self.picker_cursor, self.displays.len(), -1);
                None
            }
            _ => None,
        }
    }

    fn handle_mouse_raw(&mut self, ev: MouseEvent) -> Option<Cmd> {
        let len = self.caps.as_ref().map(|c| c.features.len()).unwrap_or(0);
        if len == 0 {
            return None;
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = self.raw_row_at(ev.row) {
                    self.raw_cursor = idx;
                }
                None
            }
            MouseEventKind::ScrollUp => {
                self.raw_cursor = self.raw_cursor.checked_sub(1).unwrap_or(len - 1);
                None
            }
            MouseEventKind::ScrollDown => {
                self.raw_cursor = (self.raw_cursor + 1) % len;
                None
            }
            _ => None,
        }
    }

    /// Maps a click's terminal row to a feature index in the Raw VCP
    /// table, accounting for the table's own header row (one line) and
    /// its current scroll offset (`raw_table_state`, which ratatui itself
    /// keeps up to date — see the module docs on `screens::raw`).
    fn raw_row_at(&self, row: u16) -> Option<usize> {
        let area = self.raw_table_area;
        if row < area.y || row >= area.y.saturating_add(area.height) {
            return None;
        }
        let header_rows = 1;
        let rel = row.checked_sub(area.y)?.checked_sub(header_rows)?;
        let idx = self.raw_table_state.offset() + rel as usize;
        let len = self.caps.as_ref()?.features.len();
        (idx < len).then_some(idx)
    }

    // ---- msg handling --------------------------------------------------

    pub fn handle_msg(&mut self, msg: Msg) -> Option<Cmd> {
        match msg {
            Msg::Detect(result) => self.on_detect(result),
            Msg::Probe(result) => {
                self.on_probe(result);
                None
            }
            Msg::CachedControls(ok) => {
                self.on_cached_controls(ok);
                None
            }
            Msg::LiveValue { code, result } => {
                self.on_live_value(code, result);
                None
            }
            Msg::Set { code, value, result } => {
                self.on_set(code, value, result);
                None
            }
            Msg::ActionDone { result } => {
                self.op_err = result.err().map(|e| e.to_string());
                if self.op_err.is_some() {
                    self.trigger_error_fx();
                }
                None
            }
            Msg::RawProbe(result) => {
                self.on_raw_probe(result);
                None
            }
            Msg::RawSingleProbe { code, result } => {
                self.on_raw_single_probe(code, result);
                None
            }
            Msg::RawSet { code, result } => self.on_raw_set(code, result),
        }
    }

    fn on_detect(&mut self, result: crate::backend::Result<Vec<Display>>) -> Option<Cmd> {
        self.loading = false;
        match result {
            Ok(displays) => {
                self.displays = displays;
                self.err = None;
            }
            Err(e) => {
                self.err = Some(e.to_string());
                self.trigger_error_fx();
                return None;
            }
        }
        if self.displays.is_empty() {
            return None;
        }
        if !self.display_chosen && self.displays.len() > 1 {
            // More than one display and nothing chosen yet (first launch,
            // or the previously-selected one vanished on refresh) — ask
            // instead of silently guessing which one is wanted.
            self.screen = Screen::Picker;
            self.picker_cursor = 0;
            return None;
        }
        if self.selected >= self.displays.len() {
            self.selected = 0;
        }
        self.display_chosen = true;
        self.probing = true;
        self.displays.get(self.selected).cloned().map(Cmd::Probe)
    }

    fn on_probe(&mut self, result: crate::backend::Result<commands::ProbeOk>) {
        self.probing = false;
        match result {
            Ok(ok) => {
                self.caps = Some(ok.caps);
                self.probe_err = None;
                self.sliders = ok.sliders;
                self.selectors = ok.selectors;
                self.actions = ok.actions;
                self.order = ok.order;
                self.cursor = 0;
                self.pending.clear(); // a fresh scan's values are already live
                self.trigger_entrance_fx();
            }
            Err(e) => {
                self.probe_err = Some(e.to_string());
                self.trigger_error_fx();
            }
        }
    }

    fn on_cached_controls(&mut self, ok: commands::ProbeOk) {
        self.probing = false;
        self.caps = Some(ok.caps);
        self.probe_err = None;
        self.sliders = ok.sliders;
        self.selectors = ok.selectors;
        self.actions = ok.actions;
        self.order = ok.order;
        self.cursor = 0;
        self.pending = self
            .sliders
            .iter()
            .map(|s| s.code)
            .chain(self.selectors.iter().map(|s| s.code))
            .collect();
        self.trigger_entrance_fx();
    }

    fn on_live_value(&mut self, code: u8, result: crate::backend::Result<FeatureReading>) {
        self.pending.remove(&code);
        // On error, the cached value just keeps showing rather than
        // blanking the control — a single flaky read isn't worth an error
        // banner when a reasonable last-known value is already on screen.
        let Ok(r) = result else { return };
        if !r.readable {
            return;
        }
        if let Some(s) = self.sliders.iter_mut().find(|s| s.code == code) {
            if r.continuous {
                s.value = r.current;
                s.max = r.max;
            }
            return;
        }
        if let Some(sel) = self.selectors.iter_mut().find(|s| s.code == code) {
            sel.selected = r.current as u8;
        }
    }

    fn on_set(&mut self, code: u8, value: u16, result: crate::backend::Result<()>) {
        self.op_err = result.as_ref().err().map(|e| e.to_string());
        if result.is_err() {
            self.trigger_error_fx();
            return;
        }
        if let Some(s) = self.sliders.iter_mut().find(|s| s.code == code) {
            s.value = value;
            return;
        }
        if let Some(sel) = self.selectors.iter_mut().find(|s| s.code == code) {
            sel.selected = value as u8;
        }
    }

    fn on_raw_probe(&mut self, result: crate::backend::Result<Vec<FeatureReading>>) {
        self.raw_loading = false;
        match result {
            Ok(readings) => {
                self.raw_err = None;
                self.raw_ready = true;
                self.raw_readings = readings.into_iter().map(|r| (r.code, r)).collect();
                self.raw_cursor = 0;
                self.raw_table_state = TableState::default();
            }
            Err(e) => {
                self.raw_err = Some(e.to_string());
                self.trigger_error_fx();
            }
        }
    }

    fn on_raw_single_probe(&mut self, code: u8, result: crate::backend::Result<FeatureReading>) {
        // Best-effort refresh of one row after a write — if the re-read
        // itself fails, the row just keeps showing its last known value,
        // same as the controls screen's live-value handling.
        if let Ok(r) = result {
            self.raw_readings.insert(code, r);
        }
    }

    fn on_raw_set(&mut self, code: u8, result: crate::backend::Result<()>) -> Option<Cmd> {
        self.raw_writing = false;
        match result {
            Ok(()) => {
                self.raw_write_err = None;
                Some(Cmd::RawSingleProbe {
                    display_num: self.display_num(),
                    code,
                })
            }
            Err(e) => {
                self.raw_write_err = Some(e.to_string());
                self.trigger_error_fx();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendError;
    use crate::commands::{build_controls_from_cache, CtrlKind};
    use crate::components::SelectorOption;
    use crate::vcp::{RawBytes, VcpFeature, VcpValue};
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn char_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn app_with_slider(code: u8, value: u16, max: u16) -> App {
        let mut app = App::new();
        app.loading = false;
        app.sliders = vec![Slider::new(code, "Test", value, max)];
        app.order = vec![CtrlRef {
            kind: CtrlKind::Slider,
            idx: 0,
        }];
        app.cursor = 0;
        app
    }

    fn app_with_selector(code: u8, options: Vec<SelectorOption>, current: u8) -> App {
        let mut app = App::new();
        app.loading = false;
        app.selectors = vec![Selector::new(code, "Test", options, current)];
        app.order = vec![CtrlRef {
            kind: CtrlKind::Selector,
            idx: 0,
        }];
        app.cursor = 0;
        app
    }

    fn app_with_action(code: u8, name: &str) -> App {
        let mut app = App::new();
        app.loading = false;
        app.actions = vec![Action {
            code,
            name: name.to_string(),
        }];
        app.order = vec![CtrlRef {
            kind: CtrlKind::Action,
            idx: 0,
        }];
        app.cursor = 0;
        app
    }

    fn recognized_feature(code: u8, name: &str) -> VcpFeature {
        VcpFeature {
            code,
            name: name.to_string(),
            recognized: true,
            manufacturer_specific: false,
            values: Vec::new(),
        }
    }

    fn raw_screen_app_with_two_features() -> App {
        let mut app = App::new();
        app.loading = false;
        app.screen = Screen::Raw;
        app.caps = Some(Capabilities {
            model: String::new(),
            mccs_version: String::new(),
            features: vec![
                recognized_feature(0x10, "Brightness"),
                VcpFeature {
                    code: 0x4D,
                    name: "Unrecognized feature".to_string(),
                    recognized: false,
                    manufacturer_specific: false,
                    values: Vec::new(),
                },
            ],
        });
        app.raw_ready = true;
        app
    }

    fn sample_cache() -> cache::MonitorCache {
        cache::MonitorCache {
            version: 0,
            capabilities: Capabilities {
                model: String::new(),
                mccs_version: String::new(),
                features: vec![
                    recognized_feature(0x10, "Brightness"),
                    VcpFeature {
                        code: 0x14,
                        name: "Select color preset".to_string(),
                        recognized: true,
                        manufacturer_specific: false,
                        values: vec![
                            VcpValue {
                                code: 0x05,
                                name: "6500 K".to_string(),
                            },
                            VcpValue {
                                code: 0x08,
                                name: "9300 K".to_string(),
                            },
                        ],
                    },
                    recognized_feature(0x04, "Restore factory defaults"),
                ],
            },
            sliders: vec![cache::CachedSlider {
                code: 0x10,
                max: 100,
                value: 80,
            }],
            selectors: vec![cache::CachedSelector {
                code: 0x14,
                selected: 0x05,
            }],
            action_codes: vec![0x04],
        }
    }

    // ---- slider adjust / set lifecycle ---------------------------------

    #[test]
    fn right_key_issues_set_cmd_but_does_not_commit_yet() {
        let mut app = app_with_slider(0x12, 70, 100);
        let cmd = app.handle_key(char_key('l'));
        assert!(matches!(cmd, Some(Cmd::Set { .. })), "expected a Set cmd");
        assert_eq!(app.sliders[0].value, 70, "must not change optimistically before confirmation");
    }

    #[test]
    fn set_msg_success_commits_value() {
        let mut app = app_with_slider(0x12, 70, 100);
        app.handle_msg(Msg::Set {
            code: 0x12,
            value: 75,
            result: Ok(()),
        });
        assert_eq!(app.sliders[0].value, 75);
        assert!(app.op_err.is_none());
    }

    #[test]
    fn set_msg_failure_leaves_value_unchanged_and_surfaces_error() {
        // Mirrors what actually happens on real hardware: a write can be
        // accepted by the transport but silently rejected by the panel, so
        // verification fails.
        let mut app = app_with_slider(0x12, 70, 100);
        app.handle_msg(Msg::Set {
            code: 0x12,
            value: 65,
            result: Err(BackendError::msg("verification failed for feature 12")),
        });
        assert_eq!(app.sliders[0].value, 70, "value must not change when the write failed");
        assert!(app.op_err.is_some());
    }

    #[test]
    fn at_upper_bound_right_key_is_noop() {
        let mut app = app_with_slider(0x10, 100, 100);
        assert!(app.handle_key(char_key('l')).is_none());
    }

    #[test]
    fn at_lower_bound_left_key_is_noop() {
        let mut app = app_with_slider(0x10, 0, 100);
        assert!(app.handle_key(char_key('h')).is_none());
    }

    #[test]
    fn cursor_navigation_wraps() {
        let mut app = App::new();
        app.loading = false;
        app.sliders = vec![Slider::new(0x10, "A", 50, 100), Slider::new(0x12, "B", 50, 100)];
        app.order = vec![
            CtrlRef { kind: CtrlKind::Slider, idx: 0 },
            CtrlRef { kind: CtrlKind::Slider, idx: 1 },
        ];
        app.cursor = 0;

        app.handle_key(char_key('k')); // up from 0 wraps to last
        assert_eq!(app.cursor, 1);

        app.handle_key(char_key('j')); // down from last wraps to 0
        assert_eq!(app.cursor, 0);
    }

    // ---- selector adjust / set lifecycle -------------------------------

    fn input_source_options() -> Vec<SelectorOption> {
        vec![
            SelectorOption { code: 0x0f, name: "DisplayPort-1".to_string() },
            SelectorOption { code: 0x11, name: "HDMI-1".to_string() },
            SelectorOption { code: 0x12, name: "HDMI-2".to_string() },
        ]
    }

    #[test]
    fn selector_right_key_cycles_to_next_option() {
        let mut app = app_with_selector(0x60, input_source_options(), 0x0f);
        let cmd = app.handle_key(char_key('l'));
        assert!(matches!(cmd, Some(Cmd::Set { .. })));
        assert_eq!(app.selectors[0].selected, 0x0f, "must not change optimistically before confirmation");
    }

    #[test]
    fn selector_set_msg_success_commits_selection() {
        let mut app = app_with_selector(0x60, input_source_options(), 0x0f);
        app.handle_msg(Msg::Set {
            code: 0x60,
            value: 0x11,
            result: Ok(()),
        });
        assert_eq!(app.selectors[0].selected, 0x11);
    }

    #[test]
    fn selector_single_option_is_noop() {
        let mut app = app_with_selector(
            0x14,
            vec![SelectorOption { code: 0x05, name: "Only Option".to_string() }],
            0x05,
        );
        assert!(app.handle_key(char_key('l')).is_none());
    }

    // ---- action confirmation flow --------------------------------------

    #[test]
    fn action_enter_opens_confirmation() {
        let mut app = app_with_action(0x04, "Restore factory defaults");
        let cmd = app.handle_key(key(KeyCode::Enter));
        assert!(app.confirming);
        assert!(cmd.is_none(), "opening the prompt should not itself issue a command");
    }

    #[test]
    fn action_left_right_do_nothing() {
        // Actions have no value to adjust — left/right must be no-ops.
        let mut app = app_with_action(0x04, "Restore factory defaults");
        assert!(app.handle_key(char_key('l')).is_none());
        assert!(app.handle_key(char_key('h')).is_none());
    }

    #[test]
    fn confirming_y_confirms_and_issues_action_cmd() {
        let mut app = app_with_action(0x04, "Restore factory defaults");
        app.confirming = true;
        app.confirm_action_idx = 0;

        let cmd = app.handle_key(char_key('y'));
        assert!(!app.confirming);
        assert!(matches!(cmd, Some(Cmd::Action { .. })));
    }

    #[test]
    fn confirming_n_cancels_without_issuing_cmd() {
        let mut app = app_with_action(0x04, "Restore factory defaults");
        app.confirming = true;
        app.confirm_action_idx = 0;

        let cmd = app.handle_key(char_key('n'));
        assert!(!app.confirming);
        assert!(cmd.is_none());
    }

    #[test]
    fn confirming_other_keys_are_swallowed() {
        // Nothing but y/n/esc/quit should have any effect while a
        // destructive action is pending confirmation.
        let mut app = app_with_action(0x04, "Restore factory defaults");
        app.confirming = true;
        app.confirm_action_idx = 0;

        let cmd = app.handle_key(char_key('j'));
        assert!(app.confirming, "confirmation should remain open for an unrelated key");
        assert!(cmd.is_none());
    }

    #[test]
    fn action_msg_surfaces_error() {
        let mut app = app_with_action(0x04, "Restore factory defaults");
        app.handle_msg(Msg::ActionDone {
            result: Err(BackendError::msg("exit status 1")),
        });
        assert!(app.op_err.is_some());
    }

    // ---- raw screen entry ----------------------------------------------

    #[test]
    fn v_key_switches_to_raw_screen_and_triggers_probe() {
        let mut app = App::new();
        app.loading = false;
        app.caps = Some(Capabilities {
            model: String::new(),
            mccs_version: String::new(),
            features: vec![recognized_feature(0x10, "Brightness")],
        });

        let cmd = app.handle_key(char_key('v'));
        assert_eq!(app.screen, Screen::Raw);
        assert!(matches!(cmd, Some(Cmd::RawProbe { .. })));
    }

    #[test]
    fn v_key_no_caps_yet_no_probe_cmd() {
        let mut app = App::new(); // caps is None (still loading/probing)
        let cmd = app.handle_key(char_key('v'));
        assert_eq!(app.screen, Screen::Raw, "should switch to Raw even without caps yet");
        assert!(cmd.is_none());
    }

    #[test]
    fn v_key_already_scanned_no_redundant_probe() {
        let mut app = App::new();
        app.caps = Some(Capabilities {
            model: String::new(),
            mccs_version: String::new(),
            features: vec![recognized_feature(0x10, "Brightness")],
        });
        app.raw_ready = true;

        assert!(app.handle_key(char_key('v')).is_none());
    }

    #[test]
    fn raw_screen_esc_returns_to_controls() {
        let mut app = App::new();
        app.screen = Screen::Raw;
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Controls);
    }

    #[test]
    fn raw_screen_v_key_toggles_back() {
        let mut app = App::new();
        app.screen = Screen::Raw;
        app.handle_key(char_key('v'));
        assert_eq!(app.screen, Screen::Controls);
    }

    #[test]
    fn raw_screen_r_triggers_rescan() {
        let mut app = App::new();
        app.screen = Screen::Raw;
        app.caps = Some(Capabilities {
            model: String::new(),
            mccs_version: String::new(),
            features: vec![recognized_feature(0x10, "Brightness")],
        });
        app.raw_ready = true; // even if already scanned once, 'r' forces a fresh probe

        assert!(matches!(app.handle_key(char_key('r')), Some(Cmd::RawProbe { .. })));
    }

    #[test]
    fn raw_probe_msg_populates_screen() {
        let mut app = App::new();
        app.caps = Some(Capabilities {
            model: String::new(),
            mccs_version: String::new(),
            features: vec![VcpFeature {
                code: 0x4D,
                name: "Unrecognized feature".to_string(),
                recognized: false,
                manufacturer_specific: false,
                values: Vec::new(),
            }],
        });
        app.raw_loading = true;

        app.handle_msg(Msg::RawProbe(Ok(vec![FeatureReading {
            code: 0x4D,
            readable: true,
            raw: Some(RawBytes { mh: 0xFF, ml: 0xFF, sh: 0x78, sl: 0x33 }),
            ..Default::default()
        }])));

        assert!(!app.raw_loading);
        assert!(app.raw_ready);
    }

    // ---- cache-hit control reconstruction -------------------------------

    #[test]
    fn build_controls_from_cache_reconstructs_all_three_kinds() {
        let (sliders, selectors, actions, order) = build_controls_from_cache(&sample_cache());

        assert_eq!(sliders.len(), 1);
        assert_eq!(sliders[0].value, 80);
        assert_eq!(sliders[0].max, 100);

        assert_eq!(selectors.len(), 1);
        assert_eq!(selectors[0].selected, 0x05);
        assert_eq!(selectors[0].options.len(), 2);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].code, 0x04);

        // Capabilities order is 0x10, 0x14, 0x04 — order must follow that,
        // not cache insertion order.
        assert_eq!(order.len(), 3);
        assert_eq!(order[0].kind, CtrlKind::Slider);
        assert_eq!(order[1].kind, CtrlKind::Selector);
        assert_eq!(order[2].kind, CtrlKind::Action);
    }

    #[test]
    fn cached_controls_msg_marks_sliders_and_selectors_pending_not_actions() {
        let mut app = App::new();
        let (sliders, selectors, actions, order) = build_controls_from_cache(&sample_cache());

        app.handle_msg(Msg::CachedControls(commands::ProbeOk {
            caps: sample_cache().capabilities,
            sliders,
            selectors,
            actions,
            order,
        }));

        assert!(app.pending.contains(&0x10), "cached slider should be pending confirmation");
        assert!(app.pending.contains(&0x14), "cached selector should be pending confirmation");
        assert!(!app.pending.contains(&0x04), "an action has no value to confirm — must never be pending");
    }

    #[test]
    fn live_value_msg_success_updates_value_and_clears_pending() {
        let mut app = app_with_slider(0x10, 80, 100);
        app.pending.insert(0x10);

        app.handle_msg(Msg::LiveValue {
            code: 0x10,
            result: Ok(FeatureReading {
                code: 0x10,
                readable: true,
                continuous: true,
                current: 95,
                max: 100,
                ..Default::default()
            }),
        });

        assert_eq!(app.sliders[0].value, 95, "live value should replace the cached one");
        assert!(!app.pending.contains(&0x10));
    }

    #[test]
    fn live_value_msg_error_keeps_cached_value_but_clears_pending() {
        let mut app = app_with_slider(0x10, 80, 100);
        app.pending.insert(0x10);

        app.handle_msg(Msg::LiveValue {
            code: 0x10,
            result: Err(BackendError::msg("transient failure")),
        });

        assert_eq!(app.sliders[0].value, 80, "a flaky read must not blank out the cached value");
        assert!(!app.pending.contains(&0x10), "pending must clear even when the live read failed");
    }

    #[test]
    fn probe_msg_fresh_scan_has_no_pending() {
        let mut app = App::new();
        app.pending.insert(0x10); // leftover from a previous cached render, shouldn't survive

        app.handle_msg(Msg::Probe(Ok(commands::ProbeOk {
            caps: Capabilities {
                model: String::new(),
                mccs_version: String::new(),
                features: vec![recognized_feature(0x10, "Brightness")],
            },
            sliders: vec![Slider::new(0x10, "Brightness", 80, 100)],
            selectors: Vec::new(),
            actions: Vec::new(),
            order: vec![CtrlRef { kind: CtrlKind::Slider, idx: 0 }],
        })));

        assert!(app.pending.is_empty(), "pending must be empty after a fresh (non-cached) scan");
    }

    // ---- effects ----------------------------------------------------------

    #[test]
    fn successful_probe_triggers_entrance_effect() {
        let mut app = App::new();
        assert!(!app.effects.is_running(), "nothing queued yet");

        app.handle_msg(Msg::Probe(Ok(commands::ProbeOk {
            caps: Capabilities {
                model: String::new(),
                mccs_version: String::new(),
                features: vec![recognized_feature(0x10, "Brightness")],
            },
            sliders: vec![Slider::new(0x10, "Brightness", 80, 100)],
            selectors: Vec::new(),
            actions: Vec::new(),
            order: vec![CtrlRef { kind: CtrlKind::Slider, idx: 0 }],
        })));

        assert!(app.effects.is_running(), "a successful probe should queue an entrance effect");
    }

    #[test]
    fn failed_detect_triggers_error_effect() {
        let mut app = App::new();
        app.handle_msg(Msg::Detect(Err(BackendError::msg("no displays"))));
        assert!(app.effects.is_running(), "a newly-surfaced error should queue a flash effect");
    }

    #[test]
    fn failed_set_triggers_error_effect() {
        let mut app = app_with_slider(0x10, 50, 100);
        app.handle_msg(Msg::Set {
            code: 0x10,
            value: 55,
            result: Err(BackendError::msg("write failed")),
        });
        assert!(app.effects.is_running(), "a failed Set should queue a flash effect");
    }

    #[test]
    fn switching_screens_triggers_transition_effect() {
        let mut app = app_with_slider(0x10, 50, 100);
        app.caps = Some(Capabilities {
            model: String::new(),
            mccs_version: String::new(),
            features: vec![recognized_feature(0x10, "Brightness")],
        });
        assert!(!app.effects.is_running());

        app.handle_key(char_key('v')); // Controls -> Raw
        assert!(app.effects.is_running(), "switching to the Raw screen should queue a transition effect");
    }

    // ---- display detection / picker -------------------------------------

    #[test]
    fn detect_msg_single_display_auto_selects_and_probes() {
        let mut app = App::new();
        let cmd = app.handle_msg(Msg::Detect(Ok(vec![Display {
            number: 0,
            mfg_id: "GSM".to_string(),
            ..Default::default()
        }])));

        assert_ne!(app.screen, Screen::Picker, "a single display must never show the picker");
        assert!(app.display_chosen);
        assert!(matches!(cmd, Some(Cmd::Probe(_))));
    }

    #[test]
    fn detect_msg_multiple_displays_shows_picker_without_probing() {
        let mut app = App::new();
        let cmd = app.handle_msg(Msg::Detect(Ok(vec![
            Display { number: 0, mfg_id: "GSM".to_string(), ..Default::default() },
            Display { number: 1, mfg_id: "DEL".to_string(), ..Default::default() },
        ])));

        assert_eq!(app.screen, Screen::Picker);
        assert!(!app.display_chosen, "display_chosen must stay false until the picker is answered");
        assert!(cmd.is_none());
    }

    #[test]
    fn detect_msg_already_chosen_skips_picker_on_refresh() {
        // Simulates pressing 'r' after already picking display 1: a
        // refresh re-detects but must not bounce back into the picker or
        // back to display 0.
        let mut app = App::new();
        app.display_chosen = true;
        app.selected = 1;

        let cmd = app.handle_msg(Msg::Detect(Ok(vec![
            Display { number: 0, mfg_id: "GSM".to_string(), ..Default::default() },
            Display { number: 1, mfg_id: "DEL".to_string(), ..Default::default() },
        ])));

        assert_ne!(app.screen, Screen::Picker, "a previously-chosen display must not re-trigger the picker");
        assert_eq!(app.selected, 1, "previous choice must be preserved");
        assert!(matches!(cmd, Some(Cmd::Probe(_))));
    }

    #[test]
    fn picker_enter_selects_and_starts_probe() {
        let mut app = App::new();
        app.screen = Screen::Picker;
        app.displays = vec![
            Display { number: 0, mfg_id: "GSM".to_string(), ..Default::default() },
            Display { number: 1, mfg_id: "DEL".to_string(), ..Default::default() },
        ];
        app.picker_cursor = 1;

        let cmd = app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Controls);
        assert_eq!(app.selected, 1);
        assert!(app.display_chosen);
        assert!(matches!(cmd, Some(Cmd::Probe(_))));
    }

    #[test]
    fn picker_up_down_moves_cursor_and_wraps() {
        let mut app = App::new();
        app.screen = Screen::Picker;
        app.displays = vec![
            Display::default(),
            Display::default(),
            Display::default(),
        ];

        app.handle_key(char_key('k')); // up from 0 wraps to last
        assert_eq!(app.picker_cursor, 2);

        app.handle_key(char_key('j')); // down from last wraps to 0
        assert_eq!(app.picker_cursor, 0);
    }

    #[test]
    fn controls_screen_d_key_switches_to_picker_when_multiple_displays() {
        let mut app = App::new();
        app.displays = vec![Display::default(), Display::default()];
        app.selected = 1;
        app.display_chosen = true;

        app.handle_key(char_key('D'));
        assert_eq!(app.screen, Screen::Picker);
        assert_eq!(app.picker_cursor, 1, "should start on the currently selected display");
    }

    #[test]
    fn controls_screen_d_key_is_noop_with_single_display() {
        let mut app = App::new();
        app.displays = vec![Display::default()];
        app.display_chosen = true;

        let cmd = app.handle_key(char_key('D'));
        assert_ne!(app.screen, Screen::Picker, "'D' must be a no-op with only one display");
        assert!(cmd.is_none());
    }

    // ---- raw screen navigation / editing --------------------------------

    #[test]
    fn raw_screen_down_moves_cursor_and_wraps() {
        let mut app = raw_screen_app_with_two_features();
        app.handle_key(char_key('j'));
        assert_eq!(app.raw_cursor, 1);
        app.handle_key(char_key('j'));
        assert_eq!(app.raw_cursor, 0);
    }

    #[test]
    fn raw_screen_up_wraps_to_last() {
        let mut app = raw_screen_app_with_two_features();
        app.handle_key(char_key('k'));
        assert_eq!(app.raw_cursor, 1);
    }

    #[test]
    fn raw_screen_e_key_enters_edit_mode() {
        let mut app = raw_screen_app_with_two_features();
        let cmd = app.handle_key(char_key('e'));
        assert!(app.raw_editing);
        assert!(cmd.is_none());
    }

    #[test]
    fn raw_editing_digits_append_to_input() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_editing = true;
        app.handle_key(char_key('7'));
        app.handle_key(char_key('5'));
        assert_eq!(app.raw_edit_input, "75");
    }

    #[test]
    fn raw_editing_non_digits_are_ignored() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_editing = true;
        app.handle_key(char_key('x'));
        assert_eq!(app.raw_edit_input, "");
    }

    #[test]
    fn raw_editing_backspace_deletes_last_digit() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_editing = true;
        app.raw_edit_input = "12".to_string();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.raw_edit_input, "1");
    }

    #[test]
    fn raw_editing_esc_cancels() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_editing = true;
        app.raw_edit_input = "42".to_string();
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.raw_editing);
        assert_eq!(app.raw_edit_input, "");
    }

    #[test]
    fn raw_editing_enter_with_valid_value_moves_to_confirming() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_editing = true;
        app.raw_edit_input = "42".to_string();

        let cmd = app.handle_key(key(KeyCode::Enter));
        assert!(!app.raw_editing);
        assert!(app.raw_confirming);
        assert_eq!(app.raw_confirm_value, 42);
        assert!(cmd.is_none());
    }

    #[test]
    fn raw_editing_enter_with_empty_input_sets_error() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_editing = true;

        app.handle_key(key(KeyCode::Enter));
        assert!(app.raw_editing, "should remain in edit mode after an invalid value");
        assert!(!app.raw_edit_err.is_empty());
    }

    #[test]
    fn raw_editing_enter_with_out_of_range_value_sets_error() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_editing = true;
        app.raw_edit_input = "999999".to_string();

        app.handle_key(key(KeyCode::Enter));
        assert!(!app.raw_confirming, "an out-of-range value must be rejected, not sent to confirmation");
        assert!(!app.raw_edit_err.is_empty());
    }

    #[test]
    fn raw_confirming_y_issues_raw_set_cmd() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_cursor = 0; // recognized code 0x10
        app.raw_confirming = true;
        app.raw_confirm_value = 50;

        let cmd = app.handle_key(char_key('y'));
        assert!(!app.raw_confirming);
        assert!(app.raw_writing, "expected raw_writing=true while the write is in flight");
        assert!(matches!(cmd, Some(Cmd::RawSet { .. })));
    }

    #[test]
    fn raw_confirming_n_cancels_without_issuing_cmd() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_confirming = true;
        app.raw_confirm_value = 50;

        let cmd = app.handle_key(char_key('n'));
        assert!(!app.raw_confirming);
        assert!(cmd.is_none());
    }

    #[test]
    fn raw_confirming_other_keys_are_swallowed() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_confirming = true;
        app.raw_confirm_value = 50;

        let cmd = app.handle_key(char_key('j'));
        assert!(app.raw_confirming, "confirmation should remain open for an unrelated key");
        assert!(cmd.is_none());
    }

    #[test]
    fn raw_set_msg_success_clears_writing_and_triggers_refresh() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_writing = true;

        let cmd = app.handle_msg(Msg::RawSet { code: 0x10, result: Ok(()) });
        assert!(!app.raw_writing);
        assert!(app.raw_write_err.is_none());
        assert!(
            matches!(cmd, Some(Cmd::RawSingleProbe { .. })),
            "expected a follow-up single-code probe to refresh the row"
        );
    }

    #[test]
    fn raw_set_msg_failure_surfaces_error() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_writing = true;

        app.handle_msg(Msg::RawSet {
            code: 0x10,
            result: Err(BackendError::msg("verification failed")),
        });
        assert!(!app.raw_writing);
        assert!(app.raw_write_err.is_some());
    }

    #[test]
    fn raw_single_probe_msg_updates_reading() {
        let mut app = raw_screen_app_with_two_features();

        app.handle_msg(Msg::RawSingleProbe {
            code: 0x10,
            result: Ok(FeatureReading {
                code: 0x10,
                readable: true,
                continuous: true,
                current: 55,
                max: 100,
                ..Default::default()
            }),
        });

        let r = app.raw_readings.get(&0x10).expect("expected raw_readings[0x10] to be populated");
        assert_eq!(r.current, 55);
    }

    // ---- mouse handling -------------------------------------------------

    fn mouse(kind: MouseEventKind, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn left_click(row: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), row)
    }

    /// Same as `left_click`/`mouse`, but with an explicit column — needed
    /// for anything that depends on *where* in the row the pointer was,
    /// i.e. a slider's bar.
    fn mouse_at(kind: MouseEventKind, row: u16, col: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click_at(row: u16, col: u16) -> MouseEvent {
        mouse_at(MouseEventKind::Down(MouseButton::Left), row, col)
    }

    fn drag_at(row: u16, col: u16) -> MouseEvent {
        mouse_at(MouseEventKind::Drag(MouseButton::Left), row, col)
    }

    /// The bar's inclusive column range for `probe`, found by scanning
    /// rather than hardcoding `Slider`'s private layout constants —
    /// keeps these tests correct even if the bar's position/width ever
    /// changes (it already has once, when the name column widened).
    fn bar_col_range(probe: &Slider) -> (u16, u16) {
        let cols: Vec<u16> = (0..200).filter(|&c| probe.value_at_column(c).is_some()).collect();
        (*cols.first().expect("slider should have a non-empty bar"), *cols.last().unwrap())
    }

    #[test]
    fn click_on_slider_row_before_the_bar_only_focuses() {
        let mut app = app_with_slider(0x10, 50, 100);
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];
        app.cursor = 99; // deliberately wrong, to prove the click corrects it

        // Column 0 lands in the name label, well before the bar starts
        // (see components::slider's BAR_START_COL/its own tests) — click
        // there should focus, not set a value.
        let cmd = app.handle_mouse(left_click(0));
        assert_eq!(app.cursor, 0);
        assert!(cmd.is_none(), "clicking the label shouldn't set a value");
        assert_eq!(app.sliders[0].value, 50);
    }

    #[test]
    fn click_inside_slider_bar_sets_the_value_it_maps_to() {
        let mut app = app_with_slider(0x10, 0, 100);
        app.click_origin_row = 0;
        app.click_origin_col = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        // The exact column->value mapping is components::Slider's own
        // concern (see its tests) — this only needs to check that a
        // click *inside* the bar reaches it and issues the value it
        // reports, not re-derive the geometry.
        // The slider starts at value 0, so pick a column comfortably
        // inside the bar but away from its very first cell — that one
        // also maps to 0, which would make the click a no-op (nothing
        // *changed*) rather than exercising an actual Set.
        let probe = Slider::new(0x10, "Test", 0, 100);
        let col = bar_col_range(&probe).0 + 10;
        let expected = probe.value_at_column(col).expect("test's column must land inside the bar");
        assert_ne!(expected, 0, "test's column must map to a value different from the slider's starting value");

        let cmd = app.handle_mouse(click_at(0, col));
        assert!(matches!(cmd, Some(Cmd::Set { value, .. }) if value == expected));
    }

    #[test]
    fn click_past_slider_bar_on_the_value_text_only_focuses() {
        let mut app = app_with_slider(0x10, 50, 100);
        app.click_origin_row = 0;
        app.click_origin_col = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        let probe = Slider::new(0x10, "Test", 0, 100);
        let col = bar_col_range(&probe).1 + 5; // comfortably past the bar's right edge, on "▌ NNN"
        assert_eq!(probe.value_at_column(col), None, "test's column must land past the bar");

        let cmd = app.handle_mouse(click_at(0, col));
        assert!(cmd.is_none());
        assert_eq!(app.sliders[0].value, 50);
    }

    #[test]
    fn drag_across_slider_bar_updates_value_continuously() {
        let mut app = app_with_slider(0x10, 0, 100);
        app.click_origin_row = 0;
        app.click_origin_col = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        let probe = Slider::new(0x10, "Test", 0, 100);
        let (first, last) = bar_col_range(&probe);
        let (col_a, col_b) = (first + 2, last - 2);
        let value_a = probe.value_at_column(col_a).unwrap();
        let value_b = probe.value_at_column(col_b).unwrap();
        assert_ne!(value_a, value_b, "test's two columns must map to different values");

        let cmd_a = app.handle_mouse(drag_at(0, col_a));
        assert!(matches!(cmd_a, Some(Cmd::Set { value, .. }) if value == value_a));

        // A real drag would have its value committed by `Msg::Set`
        // between these two points — do that by hand so the second drag
        // is compared against the updated value, not the stale one.
        app.sliders[0].value = value_a;

        let cmd_b = app.handle_mouse(drag_at(0, col_b));
        assert!(matches!(cmd_b, Some(Cmd::Set { value, .. }) if value == value_b));
    }

    #[test]
    fn drag_off_the_bar_is_a_noop() {
        let mut app = app_with_slider(0x10, 50, 100);
        app.click_origin_row = 0;
        app.click_origin_col = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        let cmd = app.handle_mouse(drag_at(0, 0)); // column 0: before the bar
        assert!(cmd.is_none());
        assert_eq!(app.sliders[0].value, 50);
    }

    #[test]
    fn drag_over_a_selector_is_a_noop() {
        let mut app = app_with_selector(0x60, input_source_options(), 0x0f);
        app.click_origin_row = 0;
        app.click_origin_col = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        let cmd = app.handle_mouse(drag_at(0, 30));
        assert!(cmd.is_none(), "drag only ever does something on a slider's bar");
        assert_eq!(app.selectors[0].selected, 0x0f);
    }

    #[test]
    fn click_on_selector_row_advances_to_next_option() {
        let mut app = app_with_selector(0x60, input_source_options(), 0x0f);
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        let cmd = app.handle_mouse(left_click(0));
        assert_eq!(app.cursor, 0);
        assert!(matches!(cmd, Some(Cmd::Set { .. })), "expected a Set cmd");
        assert_eq!(
            app.selectors[0].selected, 0x0f,
            "must not change optimistically before confirmation, same as the key path"
        );
    }

    #[test]
    fn click_on_action_row_opens_confirmation() {
        let mut app = app_with_action(0x04, "Restore factory defaults");
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        let cmd = app.handle_mouse(left_click(0));
        assert!(app.confirming);
        assert_eq!(app.confirm_action_idx, 0);
        assert!(cmd.is_none(), "opening the confirm prompt issues no cmd yet");
    }

    #[test]
    fn click_outside_click_targets_is_a_noop() {
        let mut app = app_with_slider(0x10, 50, 100);
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];
        app.cursor = 0;

        let cmd = app.handle_mouse(left_click(5)); // past the end of click_targets
        assert!(cmd.is_none());
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn scroll_over_a_slider_row_only_moves_cursor_never_adjusts_value() {
        let mut app = App::new();
        app.loading = false;
        app.sliders = vec![Slider::new(0x10, "A", 50, 100), Slider::new(0x12, "B", 50, 100)];
        app.order = vec![
            CtrlRef { kind: CtrlKind::Slider, idx: 0 },
            CtrlRef { kind: CtrlKind::Slider, idx: 1 },
        ];
        app.cursor = 0;
        // A real click target *is* present under row 0 this time (unlike
        // the header/blank-line case below) — scroll must still just
        // move the cursor, never touch the value under the pointer.
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0)), Some(ClickTarget::Order(1))];

        let cmd = app.handle_mouse(mouse(MouseEventKind::ScrollDown, 0));
        assert!(cmd.is_none(), "scroll must never issue a Set, even hovering a slider");
        assert_eq!(app.cursor, 1);
        assert_eq!(app.sliders[0].value, 50, "the value under the pointer must be untouched");
    }

    #[test]
    fn scroll_off_any_control_row_moves_cursor_instead() {
        let mut app = App::new();
        app.loading = false;
        app.sliders = vec![Slider::new(0x10, "A", 50, 100), Slider::new(0x12, "B", 50, 100)];
        app.order = vec![
            CtrlRef { kind: CtrlKind::Slider, idx: 0 },
            CtrlRef { kind: CtrlKind::Slider, idx: 1 },
        ];
        app.cursor = 0;
        // No click target at all under row 0 (e.g. it's a header/blank
        // line) — scrolling there should move the cursor, not the slider
        // under it.
        app.click_origin_row = 0;
        app.click_targets = vec![None];

        let cmd = app.handle_mouse(mouse(MouseEventKind::ScrollDown, 0));
        assert!(cmd.is_none());
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn click_on_display_row_switches_display_when_multiple() {
        let mut app = App::new();
        app.loading = false;
        app.displays = vec![
            Display {
                number: 1,
                mfg_id: "AAA".into(),
                model: "One".into(),
                ..Default::default()
            },
            Display {
                number: 2,
                mfg_id: "BBB".into(),
                model: "Two".into(),
                ..Default::default()
            },
        ];
        app.display_chosen = true;
        app.selected = 0;
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Display(0)), Some(ClickTarget::Display(1))];

        let cmd = app.handle_mouse(left_click(1));
        assert!(matches!(cmd, Some(Cmd::Probe(d)) if d.number == 2), "expected a Probe cmd for display 2");
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn mouse_is_ignored_while_confirming() {
        let mut app = app_with_action(0x04, "Restore factory defaults");
        app.confirming = true;
        app.confirm_action_idx = 0;
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Order(0))];

        let cmd = app.handle_mouse(left_click(0));
        assert!(cmd.is_none(), "the destructive-action gate must stay keyboard-only");
        assert!(app.confirming, "must still be waiting on y/n");
    }

    #[test]
    fn picker_click_selects_display() {
        let mut app = App::new();
        app.loading = false;
        app.screen = Screen::Picker;
        app.displays = vec![
            Display {
                number: 1,
                mfg_id: "AAA".into(),
                ..Default::default()
            },
            Display {
                number: 2,
                mfg_id: "BBB".into(),
                ..Default::default()
            },
        ];
        app.click_origin_row = 0;
        app.click_targets = vec![Some(ClickTarget::Display(0)), Some(ClickTarget::Display(1))];

        let cmd = app.handle_mouse(left_click(1));
        assert_eq!(app.picker_cursor, 1);
        assert!(matches!(cmd, Some(Cmd::Probe(d)) if d.number == 2));
    }

    #[test]
    fn raw_screen_click_selects_row_accounting_for_scroll_offset() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_table_area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 5,
        };
        *app.raw_table_state.offset_mut() = 1; // scrolled past feature 0
        app.raw_cursor = 0;

        // Row 0 is the table's own header; row 1 (area.y + header) is the
        // first *data* row, which — with offset 1 — is feature index 1.
        let cmd = app.handle_mouse(left_click(1));
        assert!(cmd.is_none());
        assert_eq!(app.raw_cursor, 1);
    }

    #[test]
    fn raw_screen_scroll_moves_cursor_and_wraps() {
        let mut app = raw_screen_app_with_two_features();
        app.raw_cursor = 1; // last of the two features

        let cmd = app.handle_mouse(mouse(MouseEventKind::ScrollDown, 0));
        assert!(cmd.is_none());
        assert_eq!(app.raw_cursor, 0, "should wrap back to the first feature");
    }
}
