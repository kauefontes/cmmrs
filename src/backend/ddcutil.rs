//! `DdcBackend` implementation that shells out to the `ddcutil` CLI and
//! parses its text output — a straight port of the Go project's
//! `internal/ddc` package. This is today's only backend; see `backend::mod`
//! for the plan to add native ones alongside it.

use std::process::Command;

use regex::Regex;
use std::sync::LazyLock;

use crate::vcp::{Capabilities, Display, FeatureReading, RawBytes, VcpFeature, VcpValue};

use super::{BackendError, DdcBackend, Result};

pub struct DdcutilBackend;

impl DdcutilBackend {
    pub fn new() -> Self {
        DdcutilBackend
    }
}

impl Default for DdcutilBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DdcBackend for DdcutilBackend {
    fn detect(&self) -> Result<Vec<Display>> {
        detect()
    }

    fn capabilities(&self, display_num: i32) -> Result<Capabilities> {
        get_capabilities(display_num)
    }

    fn get_vcp(&self, display_num: i32, code: u8) -> Result<FeatureReading> {
        get_vcp(display_num, code)
    }

    fn set_vcp(&self, display_num: i32, code: u8, value: u16, permit_unknown: bool) -> Result<()> {
        set_vcp(display_num, code, value, permit_unknown)
    }
}

// ---- detect --------------------------------------------------------------

static DISPLAY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Display (\d+)").unwrap());
static I2C_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"I2C bus:\s+(\S+)").unwrap());
static CONNECTOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"DRM_connector:\s+(\S+)").unwrap());
static MFG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Mfg id:\s+(\S+)").unwrap());
static MODEL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Model:\s+(.*)").unwrap());
static VCP_VER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"VCP version:\s+(\S+)").unwrap());

/// Runs `ddcutil detect` and returns every valid (non-laptop) display found.
fn detect() -> Result<Vec<Display>> {
    let output = Command::new("ddcutil").arg("detect").output()?;
    let out = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);

    if !output.status.success() && out.trim().is_empty() {
        // ddcutil can return non-zero even when it printed useful output
        // (e.g. warnings about invalid displays), so only bail if we got
        // nothing at all.
        return Err(BackendError::msg("ddcutil detect: command failed"));
    }

    let mut displays = Vec::new();
    let mut cur: Option<Display> = None;

    for raw_line in out.lines() {
        let trimmed = raw_line.trim();

        if let Some(m) = DISPLAY_RE.captures(trimmed) {
            if let Some(d) = cur.take() {
                displays.push(d);
            }
            let num: i32 = m[1].parse().unwrap_or(0);
            cur = Some(Display {
                number: num,
                ..Default::default()
            });
            continue;
        }
        if trimmed.starts_with("Invalid display") {
            if let Some(d) = cur.take() {
                displays.push(d); // flush whatever valid display we had
            }
            cur = None; // start dropping the invalid one (laptop panel, etc.)
            continue;
        }
        let Some(d) = cur.as_mut() else { continue };

        if let Some(m) = I2C_RE.captures(trimmed) {
            d.bus = m[1].to_string();
        } else if let Some(m) = CONNECTOR_RE.captures(trimmed) {
            d.connector = m[1].to_string();
        } else if let Some(m) = MFG_RE.captures(trimmed) {
            d.mfg_id = m[1].to_string();
        } else if let Some(m) = MODEL_RE.captures(trimmed) {
            d.model = m[1].trim().to_string();
        } else if let Some(m) = VCP_VER_RE.captures(trimmed) {
            d.vcp_version = m[1].to_string();
        }
    }
    if let Some(d) = cur.take() {
        displays.push(d);
    }

    Ok(displays)
}

// ---- capabilities ---------------------------------------------------------

static FEATURE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Feature:\s+([0-9A-Fa-f]{2})\s+\((.+)\)$").unwrap());
static PARSED_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Values\s+\(\s*parsed\):\s*(.*)$").unwrap());
static PARSED_ENTRY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([0-9A-Fa-f]{2}):\s+(.*)$").unwrap());

fn get_capabilities(display_num: i32) -> Result<Capabilities> {
    let output = Command::new("ddcutil")
        .arg("--display")
        .arg(display_num.to_string())
        .arg("capabilities")
        .arg("--verbose")
        .output()?;
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() && out.trim().is_empty() {
        return Err(BackendError::msg("ddcutil capabilities: command failed"));
    }
    Ok(parse_capabilities(&out))
}

/// Parses the text output of `ddcutil capabilities --verbose`.
///
/// Deliberately keeps every feature code the monitor reports, including
/// ones ddcutil labels "Unrecognized feature" or "Manufacturer specific
/// feature" — those are exactly the codes a naive parser would throw away.
fn parse_capabilities(output: &str) -> Capabilities {
    let mut caps = Capabilities::default();
    let mut cur: Option<VcpFeature> = None;
    let mut in_parsed_block = false;

    macro_rules! flush {
        () => {
            if let Some(f) = cur.take() {
                caps.features.push(f);
            }
        };
    }

    for raw_line in output.lines() {
        let trimmed = raw_line.trim();

        if let Some(rest) = trimmed.strip_prefix("Model:") {
            flush!();
            caps.model = rest.trim().to_string();
            in_parsed_block = false;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("MCCS version:") {
            flush!();
            caps.mccs_version = rest.trim().to_string();
            in_parsed_block = false;
            continue;
        }

        if let Some(m) = FEATURE_RE.captures(trimmed) {
            flush!();
            let Ok(code) = u8::from_str_radix(&m[1], 16) else {
                continue;
            };
            let name = m[2].to_string();
            let manufacturer_specific = name == "Manufacturer specific feature";
            cur = Some(VcpFeature {
                code,
                recognized: name != "Unrecognized feature" && !manufacturer_specific,
                manufacturer_specific,
                name,
                values: Vec::new(),
            });
            in_parsed_block = false;
            continue;
        }

        let Some(f) = cur.as_mut() else { continue };

        if let Some(m) = PARSED_HEADER_RE.captures(trimmed) {
            let rest = m[1].trim();
            if rest.is_empty() {
                // Values follow on their own indented lines below.
                in_parsed_block = true;
                continue;
            }
            // Single-line form, e.g. "01 02 03 (interpretation unavailable)".
            for tok in rest.split_whitespace() {
                if tok.starts_with('(') {
                    break;
                }
                if let Ok(code) = u8::from_str_radix(tok, 16) {
                    f.values.push(VcpValue {
                        code,
                        name: String::new(),
                    });
                }
            }
            in_parsed_block = false;
            continue;
        }

        if in_parsed_block {
            if let Some(m) = PARSED_ENTRY_RE.captures(trimmed) {
                if let Ok(code) = u8::from_str_radix(&m[1], 16) {
                    f.values.push(VcpValue {
                        code,
                        name: m[2].trim().to_string(),
                    });
                }
                continue;
            }
            in_parsed_block = false;
        }
    }
    flush!();

    caps
}

// ---- getvcp -----------------------------------------------------------

static NOT_READABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Feature ([0-9A-Fa-f]{2}) \(([^)]*)\) is not readable").unwrap()
});
static CONTINUOUS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"VCP code 0x([0-9A-Fa-f]{2}) \(([^)]*)\): current value =\s*(\d+), max value =\s*(\d+)").unwrap()
});
static RAW_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"VCP code 0x([0-9A-Fa-f]{2}) \(([^)]*)\): mh=0x([0-9A-Fa-f]{2}), ml=0x([0-9A-Fa-f]{2}), sh=0x([0-9A-Fa-f]{2}), sl=0x([0-9A-Fa-f]{2})").unwrap()
});
static NON_CONTINUOUS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"VCP code 0x([0-9A-Fa-f]{2}) \(([^)]*)\): (.+) \(sl=0x([0-9A-Fa-f]{2})\)").unwrap()
});
static GENERIC_REPLY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"VCP code 0x([0-9A-Fa-f]{2}) \(([^)]*)\): (.+)").unwrap());

/// Runs `ddcutil getvcp <code>` for one feature and parses its reply. A
/// write-only/action feature (e.g. "restore factory defaults") is not an
/// error: it comes back as `FeatureReading { readable: false, .. }`.
fn get_vcp(display_num: i32, code: u8) -> Result<FeatureReading> {
    let output = Command::new("ddcutil")
        .arg("--display")
        .arg(display_num.to_string())
        .arg("getvcp")
        .arg(format!("{code:02x}"))
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    parse_get_vcp_reply(code, &text, output.status.success())
}

/// The pure part of `get_vcp`, split out so the shapes it has to handle can
/// be exercised with fixture text instead of a real subprocess.
fn parse_get_vcp_reply(code: u8, text: &str, ok: bool) -> Result<FeatureReading> {
    if let Some(m) = NOT_READABLE_RE.captures(text) {
        return Ok(FeatureReading {
            code,
            name: m[2].trim().to_string(),
            readable: false,
            ..Default::default()
        });
    }
    if let Some(m) = CONTINUOUS_RE.captures(text) {
        let cur: u16 = m[3].parse().unwrap_or(0);
        let max: u16 = m[4].parse().unwrap_or(0);
        return Ok(FeatureReading {
            code,
            name: m[2].trim().to_string(),
            readable: true,
            continuous: true,
            current: cur,
            max,
            ..Default::default()
        });
    }
    if let Some(m) = RAW_VALUE_RE.captures(text) {
        let mh = u8::from_str_radix(&m[3], 16).unwrap_or(0);
        let ml = u8::from_str_radix(&m[4], 16).unwrap_or(0);
        let sh = u8::from_str_radix(&m[5], 16).unwrap_or(0);
        let sl = u8::from_str_radix(&m[6], 16).unwrap_or(0);
        return Ok(FeatureReading {
            code,
            name: m[2].trim().to_string(),
            readable: true,
            current: sl as u16,
            raw: Some(RawBytes { mh, ml, sh, sl }),
            ..Default::default()
        });
    }
    if let Some(m) = NON_CONTINUOUS_RE.captures(text) {
        let sl = u8::from_str_radix(&m[4], 16).unwrap_or(0);
        return Ok(FeatureReading {
            code,
            name: m[2].trim().to_string(),
            readable: true,
            current: sl as u16,
            label: m[3].trim().to_string(),
            ..Default::default()
        });
    }
    // A handful of features (VCP Version, Active control, frequencies,
    // firmware level, usage time, ...) get bespoke formatting that matches
    // none of the shapes above. Rather than treat a successful read as an
    // error, keep whatever text it printed — losing the feature entirely
    // would be worse than an unparsed label.
    if ok {
        if let Some(m) = GENERIC_REPLY_RE.captures(text) {
            return Ok(FeatureReading {
                code,
                name: m[2].trim().to_string(),
                readable: true,
                label: m[3].trim().to_string(),
                generic: true,
                ..Default::default()
            });
        }
    }
    if !ok {
        return Err(BackendError::msg(format!(
            "ddcutil getvcp {code:02x}: {}",
            text.trim()
        )));
    }
    Err(BackendError::msg(format!(
        "ddcutil getvcp {code:02x}: unrecognized output: {}",
        text.trim()
    )))
}

// ---- setvcp -----------------------------------------------------------

/// Runs `ddcutil setvcp <code> <value>`. ddcutil reads the value back
/// afterward to verify the write took effect, so an `Err` here means the
/// monitor didn't actually change (not just that the command failed to
/// run). `permit_unknown` adds `--permit-unknown-feature`, required by
/// ddcutil to write a code it doesn't recognize (unrecognized or
/// manufacturer-specific) as a safety guard against blindly poking
/// undocumented registers.
fn set_vcp(display_num: i32, code: u8, value: u16, permit_unknown: bool) -> Result<()> {
    let mut args = vec!["--display".to_string(), display_num.to_string()];
    if permit_unknown {
        args.push("--permit-unknown-feature".to_string());
    }
    args.push("setvcp".to_string());
    args.push(format!("{code:02x}"));
    args.push(value.to_string());

    let output = Command::new("ddcutil").args(&args).output()?;
    if !output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        return Err(BackendError::msg(format!(
            "ddcutil setvcp {code:02x} {value}: {}",
            out.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- getvcp: samples captured against a real LG 29UM68 via `ddcutil
    // getvcp`, one per output shape the parser has to handle. Ported from
    // internal/ddc/getvcp_test.go in the Go original.

    const SAMPLE_CONTINUOUS: &str =
        "VCP code 0x10 (Brightness                    ): current value =   100, max value =   100\n";
    const SAMPLE_NON_CONTINUOUS_KNOWN: &str =
        "VCP code 0x14 (Select color preset           ): 6500 K (sl=0x05)\n";
    const SAMPLE_NON_CONTINUOUS_INVALID: &str =
        "VCP code 0x60 (Input Source                  ): Invalid value (sl=0x00)\n";
    const SAMPLE_RAW_UNRECOGNIZED: &str =
        "VCP code 0x4d (Unknown feature               ): mh=0xff, ml=0xff, sh=0x78, sl=0x33\n";
    const SAMPLE_RAW_MANUFACTURER: &str =
        "VCP code 0xf4 (Manufacturer Specific         ): mh=0xff, ml=0xff, sh=0x00, sl=0x1f\n";
    const SAMPLE_NOT_READABLE: &str = "Feature 04 (Restore factory defaults) is not readable\n";

    // ddcutil gives several features their own bespoke formatting that
    // matches none of the generic shapes above — these must fall back to
    // GENERIC_REPLY_RE instead of being treated as parse errors.
    const SAMPLE_FREQUENCY: &str = "VCP code 0xac (Horizontal frequency          ): 1164 hz\n";
    const SAMPLE_VERSION: &str = "VCP code 0xdf (VCP Version                   ): 2.1\n";
    const SAMPLE_HEX_VALUE: &str = "VCP code 0x52 (Active control                ): Value: 0x00\n";
    const SAMPLE_USAGE_TIME: &str = "VCP code 0xc0 (Display usage time            ): Usage time (hours) = 25985 (0x006581) mh=0xff, ml=0xff, sh=0x65, sl=0x81\n";

    #[test]
    fn getvcp_parses_continuous() {
        let m = CONTINUOUS_RE.captures(SAMPLE_CONTINUOUS).expect("CONTINUOUS_RE did not match");
        assert_eq!(&m[1], "10");
        assert_eq!(&m[2], "Brightness                    ");
        assert_eq!(&m[3], "100");
        assert_eq!(&m[4], "100");
    }

    #[test]
    fn getvcp_parses_known_enum_value() {
        let m = NON_CONTINUOUS_RE
            .captures(SAMPLE_NON_CONTINUOUS_KNOWN)
            .expect("NON_CONTINUOUS_RE did not match");
        assert_eq!(&m[3], "6500 K");
        assert_eq!(&m[4], "05");
    }

    #[test]
    fn getvcp_parses_invalid_enum_value() {
        let m = NON_CONTINUOUS_RE
            .captures(SAMPLE_NON_CONTINUOUS_INVALID)
            .expect("NON_CONTINUOUS_RE did not match");
        assert_eq!(&m[3], "Invalid value");
        assert_eq!(&m[4], "00");
    }

    #[test]
    fn getvcp_parses_raw_unrecognized() {
        let m = RAW_VALUE_RE
            .captures(SAMPLE_RAW_UNRECOGNIZED)
            .expect("RAW_VALUE_RE did not match");
        assert_eq!(&m[1], "4d");
        assert_eq!(&m[3], "ff");
        assert_eq!(&m[4], "ff");
        assert_eq!(&m[5], "78");
        assert_eq!(&m[6], "33");
        // Must not also be swallowed by the generic non-continuous pattern
        // (no parenthesized "(sl=..)" in the raw form).
        assert!(
            !NON_CONTINUOUS_RE.is_match(SAMPLE_RAW_UNRECOGNIZED),
            "NON_CONTINUOUS_RE unexpectedly matched a raw-form reply"
        );
    }

    #[test]
    fn getvcp_parses_raw_manufacturer_specific() {
        let m = RAW_VALUE_RE
            .captures(SAMPLE_RAW_MANUFACTURER)
            .expect("RAW_VALUE_RE did not match");
        assert_eq!(&m[1], "f4");
        assert_eq!(&m[6], "1f");
    }

    #[test]
    fn getvcp_falls_back_to_generic_reply_for_bespoke_formats() {
        let cases = [
            ("frequency", SAMPLE_FREQUENCY, "ac", "1164 hz"),
            ("version", SAMPLE_VERSION, "df", "2.1"),
            ("hex value", SAMPLE_HEX_VALUE, "52", "Value: 0x00"),
            (
                "usage time",
                SAMPLE_USAGE_TIME,
                "c0",
                "Usage time (hours) = 25985 (0x006581) mh=0xff, ml=0xff, sh=0x65, sl=0x81",
            ),
        ];
        for (name, sample, want_code, want_label) in cases {
            // None of the specific patterns should claim these lines...
            assert!(
                !CONTINUOUS_RE.is_match(sample) && !RAW_VALUE_RE.is_match(sample) && !NON_CONTINUOUS_RE.is_match(sample),
                "{name}: a specific pattern unexpectedly matched {sample:?}"
            );
            // ...but the generic fallback must, preserving the full text.
            let m = GENERIC_REPLY_RE
                .captures(sample)
                .unwrap_or_else(|| panic!("{name}: GENERIC_REPLY_RE did not match {sample:?}"));
            assert_eq!(&m[1], want_code, "{name}: code");
            assert_eq!(&m[3], want_label, "{name}: label");
        }
    }

    #[test]
    fn getvcp_detects_not_readable() {
        let m = NOT_READABLE_RE
            .captures(SAMPLE_NOT_READABLE)
            .expect("NOT_READABLE_RE did not match");
        assert_eq!(&m[1], "04");
        assert_eq!(&m[2], "Restore factory defaults");
    }

    #[test]
    fn parse_get_vcp_reply_generic_fallback_is_flagged() {
        // Readings that fall back to the catch-all format never get a real
        // value code parsed — `current` must stay 0 *and* be recognizable
        // as "never parsed" rather than "genuinely zero", so a caller
        // (like the Raw VCP table) doesn't print a fabricated "(0x00)"
        // next to it.
        let r = parse_get_vcp_reply(0xac, SAMPLE_FREQUENCY, true).unwrap();
        assert!(r.generic, "expected generic=true for a bespoke-format reply");
        assert_eq!(r.current, 0, "current should stay 0 (never parsed)");
        assert_eq!(r.label, "1164 hz");
    }

    #[test]
    fn parse_get_vcp_reply_continuous_is_not_flagged_generic() {
        let r = parse_get_vcp_reply(0x10, SAMPLE_CONTINUOUS, true).unwrap();
        assert!(!r.generic, "a continuous reading must not be marked generic");
    }

    #[test]
    fn parse_get_vcp_reply_known_enum_is_not_flagged_generic() {
        let r = parse_get_vcp_reply(0x14, SAMPLE_NON_CONTINUOUS_KNOWN, true).unwrap();
        assert!(!r.generic, "a known non-continuous reading must not be marked generic");
    }

    // ---- capabilities: captured from the same monitor via `ddcutil
    // --display 1 capabilities --verbose`. Ported from
    // internal/ddc/capabilities_test.go.

    const SAMPLE_CAPABILITIES: &str = r#"Model: Not specified
MCCS version: 2.1
VCP Features:
   Feature: 02 (New control value)
   Feature: 04 (Restore factory defaults)
   Feature: 05 (Restore factory brightness/contrast defaults)
   Feature: 08 (Restore color defaults)
   Feature: 10 (Brightness)
   Feature: 12 (Contrast)
   Feature: 14 (Select color preset)
      Values (unparsed): 05 08 0B
      Values (  parsed):
         05: 6500 K
         08: 9300 K
         0b: User 1
   Feature: 16 (Video gain: Red)
   Feature: 18 (Video gain: Green)
   Feature: 1A (Video gain: Blue)
   Feature: 52 (Active control)
   Feature: 60 (Input Source)
      Values (unparsed):  11 12 0F
      Values (  parsed):
         11: HDMI-1
         12: HDMI-2
         0f: DisplayPort-1
   Feature: A4 (Turn the selected window operation on/off)
      Values (unparsed): 01 02 03
      Values (  parsed): 01 02 03 (interpretation unavailable)
   Feature: AC (Horizontal frequency)
   Feature: AE (Vertical frequency)
   Feature: B2 (Flat panel sub-pixel layout)
   Feature: B6 (Display technology type)
   Feature: C0 (Display usage time)
   Feature: C6 (Application enable key)
   Feature: C8 (Display controller type)
   Feature: C9 (Display firmware level)
   Feature: D6 (Power mode)
      Values (unparsed): 01 04
      Values (  parsed):
         01: DPM: On,  DPMS: Off
         04: DPM: Off, DPMS: Off
   Feature: DF (VCP Version)
   Feature: 62 (Audio speaker volume)
   Feature: 8D (Audio Mute)
   Feature: F4 (Manufacturer specific feature)
   Feature: F5 (Manufacturer specific feature)
      Values (unparsed): 00 01 02 03 04
      Values (  parsed): 00 01 02 03 04 (interpretation unavailable)
   Feature: F6 (Manufacturer specific feature)
      Values (unparsed): 00 01 02
      Values (  parsed): 00 01 02 (interpretation unavailable)
   Feature: 4D (Unrecognized feature)
   Feature: 4E (Unrecognized feature)
   Feature: 4F (Unrecognized feature)
   Feature: 15 (Unrecognized feature)
      Values (unparsed): 01 11 13 14 28 29 32 48
      Values (  parsed): 01 11 13 14 28 29 32 48 (interpretation unavailable)
   Feature: F7 (Manufacturer specific feature)
      Values (unparsed): 00 01 02 03
      Values (  parsed): 00 01 02 03 (interpretation unavailable)
   Feature: F8 (Manufacturer specific feature)
      Values (unparsed): 00 01
      Values (  parsed): 00 01 (interpretation unavailable)
   Feature: F9 (Manufacturer specific feature)
   Feature: FD (Manufacturer specific feature)
      Values (unparsed): 00 01
      Values (  parsed): 00 01 (interpretation unavailable)
   Feature: FE (Manufacturer specific feature)
      Values (unparsed): 00 01 02
      Values (  parsed): 00 01 02 (interpretation unavailable)
   Feature: FF (Manufacturer specific feature)
"#;

    #[test]
    fn parse_capabilities_header_fields() {
        let caps = parse_capabilities(SAMPLE_CAPABILITIES);
        assert_eq!(caps.mccs_version, "2.1");
    }

    #[test]
    fn parse_capabilities_feature_count() {
        let caps = parse_capabilities(SAMPLE_CAPABILITIES);
        assert_eq!(caps.features.len(), 38);
    }

    #[test]
    fn parse_capabilities_known_feature() {
        let caps = parse_capabilities(SAMPLE_CAPABILITIES);
        let f = caps.feature(0x10).expect("feature 0x10 (Brightness) not found");
        assert_eq!(f.name, "Brightness");
        assert!(f.recognized);
        assert!(!f.manufacturer_specific);
        assert!(!f.has_values());
    }

    #[test]
    fn parse_capabilities_multi_line_enum() {
        let caps = parse_capabilities(SAMPLE_CAPABILITIES);
        let f = caps.feature(0x60).expect("feature 0x60 (Input Source) not found");
        let want = [(0x11u8, "HDMI-1"), (0x12, "HDMI-2"), (0x0f, "DisplayPort-1")];
        assert_eq!(f.values.len(), want.len());
        for (code, name) in want {
            assert_eq!(f.value_name(code), Some(name));
        }
    }

    #[test]
    fn parse_capabilities_single_line_uninterpreted_enum() {
        let caps = parse_capabilities(SAMPLE_CAPABILITIES);
        let f = caps.feature(0xA4).expect("feature 0xA4 not found");
        assert_eq!(f.values.len(), 3);
        for v in &f.values {
            assert!(v.name.is_empty(), "value 0x{:02X}: expected no interpretation, got {:?}", v.code, v.name);
        }
    }

    #[test]
    fn parse_capabilities_unrecognized_features_are_preserved() {
        let caps = parse_capabilities(SAMPLE_CAPABILITIES);
        for code in [0x4D, 0x4E, 0x4F, 0x15] {
            let f = caps
                .feature(code)
                .unwrap_or_else(|| panic!("unrecognized feature 0x{code:02X} was dropped, not preserved"));
            assert!(!f.recognized, "feature 0x{code:02X}: recognized = true, want false");
        }
        // 0x15 additionally carries an unparsed enum — must still be kept.
        let f = caps.feature(0x15).unwrap();
        assert_eq!(f.values.len(), 8);
    }

    #[test]
    fn parse_capabilities_manufacturer_specific_features_are_preserved() {
        let caps = parse_capabilities(SAMPLE_CAPABILITIES);
        for code in [0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFD, 0xFE, 0xFF] {
            let f = caps
                .feature(code)
                .unwrap_or_else(|| panic!("manufacturer-specific feature 0x{code:02X} was dropped"));
            assert!(f.manufacturer_specific && !f.recognized, "feature 0x{code:02X}: {f:?}");
        }
    }
}
