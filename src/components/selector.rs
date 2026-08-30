use ratatui::text::{Line, Span};

use crate::components::{truncate_name, NAME_WIDTH};
use crate::styles;

/// One named value a Selector can take, e.g. `{ code: 0x11, name: "HDMI-1" }`.
#[derive(Debug, Clone)]
pub struct Option {
    pub code: u8,
    pub name: String,
}

/// A non-continuous VCP control with a monitor-declared, fully named set of
/// values, e.g. Input Source or Color Preset.
#[derive(Debug, Clone)]
pub struct Selector {
    pub code: u8,
    pub name: String,
    pub options: Vec<Option>,
    /// Current raw value; may not be among `options` — see `next_option`.
    pub selected: u8,
}

impl Selector {
    pub fn new(code: u8, name: impl Into<String>, options: Vec<Option>, current: u8) -> Self {
        Selector {
            code,
            name: name.into(),
            options,
            selected: current,
        }
    }

    fn index_of(&self, code: u8) -> std::option::Option<usize> {
        self.options.iter().position(|o| o.code == code)
    }

    /// Returns the option code the selector would move to for the given
    /// direction (+1 or -1), wrapping around. If the monitor's current
    /// value isn't among the declared options — this happens for real, some
    /// panels report a value they never advertised — cycling lands on the
    /// first option rather than guessing an offset from an unknown
    /// position.
    pub fn next_option(&self, direction: i32) -> u8 {
        if self.options.is_empty() {
            return self.selected;
        }
        let Some(idx) = self.index_of(self.selected) else {
            return self.options[0].code;
        };
        let len = self.options.len() as i32;
        let next = ((idx as i32 + direction).rem_euclid(len)) as usize;
        self.options[next].code
    }

    fn current_name(&self) -> String {
        match self.index_of(self.selected) {
            Some(idx) => self.options[idx].name.clone(),
            None => format!("unknown (0x{:02X})", self.selected),
        }
    }

    pub fn view(&self, focused: bool) -> Line<'static> {
        let (cursor, name_style) = if focused {
            ("▸ ", styles::focused_name())
        } else {
            ("  ", styles::name())
        };
        let line = Line::from(vec![
            Span::raw(cursor),
            Span::styled(format!("{:<NAME_WIDTH$}", truncate_name(&self.name, NAME_WIDTH)), name_style),
            Span::raw(format!(" ‹ {} ›", self.current_name())),
        ]);
        if focused {
            styles::with_focus_bg(line)
        } else {
            line
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_source_selector(current: u8) -> Selector {
        Selector::new(
            0x60,
            "Input Source",
            vec![
                Option {
                    code: 0x0f,
                    name: "DisplayPort-1".to_string(),
                },
                Option {
                    code: 0x11,
                    name: "HDMI-1".to_string(),
                },
                Option {
                    code: 0x12,
                    name: "HDMI-2".to_string(),
                },
            ],
            current,
        )
    }

    #[test]
    fn next_option_wraps_forward() {
        let s = input_source_selector(0x12); // HDMI-2, last option
        assert_eq!(s.next_option(1), 0x0f, "should wrap to first");
    }

    #[test]
    fn next_option_wraps_backward() {
        let s = input_source_selector(0x0f); // DisplayPort-1, first option
        assert_eq!(s.next_option(-1), 0x12, "should wrap to last");
    }

    #[test]
    fn next_option_unknown_current_value_lands_on_first() {
        // The monitor can (and on real hardware, does) report a current
        // value outside the set it advertised in capabilities.
        let s = input_source_selector(0x00);
        assert_eq!(s.next_option(1), 0x0f);
        assert_eq!(s.next_option(-1), 0x0f);
    }

    #[test]
    fn current_name_unknown_value_is_labeled() {
        let s = input_source_selector(0x00);
        assert_eq!(s.current_name(), "unknown (0x00)");
    }

    #[test]
    fn current_name_known_value() {
        let s = input_source_selector(0x11);
        assert_eq!(s.current_name(), "HDMI-1");
    }
}
