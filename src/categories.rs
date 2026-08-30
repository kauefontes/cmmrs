//! Groups VCP feature codes into named sections for the Controls screen —
//! `DISPLAY`, `COLOR`, `AUDIO`, `POWER`, `INPUT` — same spirit as
//! `backend::native::WELL_KNOWN`: a small, best-effort table of the codes
//! every DDC/CI monitor control app actually deals with, not an attempt to
//! categorize the entire MCCS spec. A code this table doesn't know about
//! still shows up, just under `Other` — nothing becomes unreachable.

/// A section on the Controls screen. Order here is render order: sections
/// appear top to bottom in this sequence, and a section with no controls
/// in it that frame is skipped entirely (not every monitor has audio).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Display,
    Color,
    Audio,
    Power,
    Input,
    Other,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Display => "DISPLAY",
            Category::Color => "COLOR",
            Category::Audio => "AUDIO",
            Category::Power => "POWER",
            Category::Input => "INPUT",
            Category::Other => "OTHER",
        }
    }
}

/// `Category::Other` is deliberately not in this table — it's the `_ =>`
/// fallback in `category_for`, not a listed entry, so every code the
/// table doesn't recognize lands there automatically.
const TABLE: &[(u8, Category)] = &[
    // Display: luminance/contrast/gain and other picture-geometry codes.
    (0x10, Category::Display), // Brightness
    (0x12, Category::Display), // Contrast
    (0x16, Category::Display), // Video Gain (Red)
    (0x18, Category::Display), // Video Gain (Green)
    (0x1a, Category::Display), // Video Gain (Blue)
    (0x6c, Category::Display), // Video Black Level (Red)
    (0x6e, Category::Display), // Video Black Level (Green)
    (0x70, Category::Display), // Video Black Level (Blue)
    (0x1e, Category::Display), // Auto Setup
    // Color: presets and fine color tuning.
    (0x14, Category::Color), // Select Color Preset
    (0x59, Category::Color), // 6-axis Saturation: Red
    (0x5a, Category::Color), // 6-axis Saturation: Yellow
    (0x5b, Category::Color), // 6-axis Saturation: Green
    (0x5c, Category::Color), // 6-axis Saturation: Cyan
    (0x5d, Category::Color), // 6-axis Saturation: Blue
    (0x5e, Category::Color), // 6-axis Saturation: Magenta
    (0x72, Category::Color), // Gamma
    // Audio.
    (0x62, Category::Audio), // Audio Speaker Volume
    (0x63, Category::Audio), // Audio Speaker Select
    (0x64, Category::Audio), // Audio Microphone Volume
    (0x8d, Category::Audio), // Audio Mute
    (0x8f, Category::Audio), // Audio Treble
    (0x91, Category::Audio), // Audio Bass
    (0x93, Category::Audio), // Audio Balance
    // Power: state and factory-reset actions.
    (0xd6, Category::Power), // Power Mode
    (0x04, Category::Power), // Restore Factory Defaults
    (0x05, Category::Power), // Restore Factory Luminance/Contrast Defaults
    (0x06, Category::Power), // Restore Factory Geometry Defaults
    (0x08, Category::Power), // Restore Factory Color Defaults
    // Input.
    (0x60, Category::Input), // Input Source
    (0xd0, Category::Input), // Output Select
];

pub fn category_for(code: u8) -> Category {
    TABLE.iter().find(|&&(c, _)| c == code).map(|&(_, cat)| cat).unwrap_or(Category::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_land_in_the_expected_section() {
        assert_eq!(category_for(0x10), Category::Display); // Brightness
        assert_eq!(category_for(0x14), Category::Color); // Select Color Preset
        assert_eq!(category_for(0x8d), Category::Audio); // Audio Mute
        assert_eq!(category_for(0xd6), Category::Power); // Power Mode
        assert_eq!(category_for(0x60), Category::Input); // Input Source
    }

    #[test]
    fn unknown_code_falls_back_to_other_rather_than_disappearing() {
        assert_eq!(category_for(0x99), Category::Other);
    }

    #[test]
    fn render_order_is_display_first_other_last() {
        assert!(Category::Display < Category::Color);
        assert!(Category::Power < Category::Input);
        assert!(Category::Input < Category::Other);
    }
}
