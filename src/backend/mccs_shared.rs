//! VESA MCCS capability-database logic shared by every *native* backend
//! (`backend::native` on Linux, `backend::macos` on macOS) — anything
//! that resolves a raw `mccs::Capabilities`/`mccs_db::Database` pair
//! (parsed from a monitor's own capability string) into names, types,
//! and `crate::vcp::Capabilities`, independent of *how* those bytes got
//! read off the wire. The `ddcutil` backend has no equivalent need for
//! this — `ddcutil` itself, not this crate, is what names features for
//! that one.
//!
//! Kept separate from any one backend specifically so a fix here (this
//! is exactly where both of `backend::native`'s early bugs lived — see
//! git history around "everything shows unrecognized" and "Mute
//! classified as a slider") applies to every native backend at once,
//! instead of needing rediscovery on each new platform.

use mccs_db::{Access, Database, Descriptor, ValueInterpretation, ValueType as DbValueType};

use crate::vcp::{Capabilities, VcpFeature, VcpValue};

pub fn to_capabilities(raw: &mccs::Capabilities, db: &Database) -> Capabilities {
    let mut caps = Capabilities {
        model: raw.model.clone().unwrap_or_default(),
        mccs_version: raw.mccs_version.map(|v| v.to_string()).unwrap_or_default(),
        features: Vec::new(),
    };

    for (&code, cap_desc) in &raw.vcp_features {
        let (name, recognized) = describe(db, code);
        let manufacturer_specific = !recognized && code >= 0xE0;
        let name = name.unwrap_or_else(|| {
            if manufacturer_specific {
                "Manufacturer specific feature".to_string()
            } else {
                "Unrecognized feature".to_string()
            }
        });
        // The monitor's own declared values win when it bothers to
        // declare any — `well_known_values` is strictly a fallback for
        // when it names the code but leaves the value list empty (Audio
        // Mute on some panels), which otherwise builds a selector with
        // nothing to cycle through (see `Selector::next_option`).
        let values = if cap_desc.values.is_empty() {
            well_known_values(code)
                .iter()
                .map(|&(v_code, v_name)| VcpValue {
                    code: v_code,
                    name: v_name.to_string(),
                })
                .collect()
        } else {
            cap_desc
                .values
                .iter()
                .map(|(&v_code, v_name)| VcpValue {
                    code: v_code,
                    name: v_name.clone().unwrap_or_default(),
                })
                .collect()
        };

        caps.features.push(VcpFeature {
            code,
            name,
            recognized,
            manufacturer_specific,
            values,
        });
    }

    caps
}

/// The handful of VCP codes every DDC/CI monitor control app leans on —
/// brightness, contrast, volume, color preset, ... — that `mccs-db`
/// 0.1.3's bundled database (`data/mccs.yml`, a small, clearly partial
/// slice of the MCCS spec) simply doesn't list. Without this, `describe`
/// would call the single most commonly-used controls "unrecognized" and
/// `get_vcp` would have no idea they're continuous vs. non-continuous,
/// which is exactly what made the home screen show most of a monitor's
/// controls as unknown instead of building sliders/selectors for them.
/// `ddcutil` doesn't have this problem — its own C database, not this
/// crate, is what names features for the `ddcutil` backend.
///
/// Kept intentionally small: this is a *fallback* for well-known,
/// unambiguous codes only, not an attempt to re-implement the MCCS
/// database. The non-continuous value labels here are a *last-resort*
/// fallback too — used only when the monitor's own capability string
/// doesn't enumerate them itself (see `to_capabilities`), which real
/// monitors do for e.g. Audio Mute: it's binary enough that some panels
/// just declare the code with no value list at all, rather than write
/// out "01 02".
const WELL_KNOWN: &[(u8, &str, Kind)] = &[
    (0x10, "Brightness", Kind::Continuous),
    (0x12, "Contrast", Kind::Continuous),
    (0x14, "Select Color Preset", Kind::NonContinuous(&[])),
    (0x16, "Video Gain (Red)", Kind::Continuous),
    (0x18, "Video Gain (Green)", Kind::Continuous),
    (0x1a, "Video Gain (Blue)", Kind::Continuous),
    (0x60, "Input Source", Kind::NonContinuous(&[])),
    (0x62, "Audio Speaker Volume", Kind::Continuous),
    (0x8d, "Audio Mute", Kind::NonContinuous(&[(0x01, "Mute"), (0x02, "Unmute")])),
    (0xd6, "Power Mode", Kind::NonContinuous(&[])),
];

enum Kind {
    Continuous,
    /// Fallback `(value, name)` pairs — empty when the code's values are
    /// too vendor/monitor-specific to guess (input source numbering,
    /// color preset sets, ...) and are left to the capability string.
    NonContinuous(&'static [(u8, &'static str)]),
}

/// The value-label fallback for `code`, if `well_known` has one — used by
/// `to_capabilities` to give a selector something to cycle through when
/// the monitor's own capability string names the code but declares no
/// values for it (see `WELL_KNOWN`'s docs).
fn well_known_values(code: u8) -> &'static [(u8, &'static str)] {
    match WELL_KNOWN.iter().find(|&&(c, _, _)| c == code) {
        Some((_, _, Kind::NonContinuous(values))) => values,
        _ => &[],
    }
}

fn well_known(code: u8) -> Option<Descriptor> {
    WELL_KNOWN.iter().find(|&&(c, _, _)| c == code).map(|&(code, name, ref kind)| Descriptor {
        name: Some(name.to_string()),
        code,
        ty: match kind {
            Kind::Continuous => DbValueType::Continuous {
                interpretation: ValueInterpretation::Continuous,
            },
            Kind::NonContinuous(values) => DbValueType::NonContinuous {
                values: values.iter().map(|&(v, n)| (v, Some(n.to_string()))).collect(),
                interpretation: ValueInterpretation::NonContinuous,
            },
        },
        ..Default::default()
    })
}

/// The descriptor for `code`, preferring the real MCCS database and
/// falling back to `well_known` for the codes it's missing.
///
/// A code the base database doesn't know but this monitor's own
/// capability string mentions still gets an entry from
/// `Database::apply_capabilities` — just with `name: None`, since the
/// capability string itself never carries human names. Worse, that
/// entry's *type* is a guess too: `apply_capabilities` calls it
/// Continuous whenever the capability string didn't enumerate any values,
/// which is flatly wrong for something like Audio Mute (binary, but
/// plenty of monitors don't bother listing "01 02" in their capability
/// string). So a plain `db.get(code).or_else(well_known)` wouldn't reach
/// the fallback (the capability-derived `Some(..)` wins every time), and
/// even patching just the name in would leave a "continuous" Mute pretending
/// to be a slider. Prefer `well_known`'s name *and* type outright — it's
/// spec-derived, not inferred from what one monitor happened to declare.
pub fn resolve(db: &Database, code: u8) -> Option<Descriptor> {
    match db.get(code).cloned() {
        // A real database entry (from `mccs.yml`, not synthesized by
        // `apply_capabilities`) is the most authoritative source there
        // is — trust it outright, even over `well_known`.
        Some(d) if d.name.is_some() => Some(d),
        Some(mut d) => {
            if let Some(wk) = well_known(code) {
                d.name = wk.name;
                d.ty = wk.ty;
            }
            Some(d)
        }
        None => well_known(code),
    }
}

/// `(name, recognized)` for a feature code — `recognized` means we know
/// what the code means (spec database or `well_known` fallback), not just
/// that this monitor's capability string mentions it, same distinction
/// `ddcutil` draws.
pub fn describe(db: &Database, code: u8) -> (Option<String>, bool) {
    match resolve(db, code).and_then(|d| d.name) {
        Some(name) => (Some(name), true),
        None => (None, false),
    }
}

/// Whether `descriptor` (from `resolve`) says its feature is write-only —
/// shared by every native backend's `get_vcp` for the same early-return
/// shape (a write-only code has no value to read back).
pub fn is_write_only(descriptor: &Descriptor) -> bool {
    descriptor.access == Access::WriteOnly
}
