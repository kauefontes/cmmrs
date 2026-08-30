//! Native Linux DDC/CI backend: talks to monitors directly over
//! `/dev/i2c-*` via the `ddc`/`ddc-i2c` crates instead of shelling out to
//! `ddcutil`. See `backend::mod` for how OS-native backends are meant to
//! slot in alongside the `ddcutil` one; this is the first of them.
//!
//! Feature-code metadata (name, continuous-vs-enum, read/write access) is
//! resolved with `mccs-caps`/`mccs-db` — the same VESA MCCS spec tables
//! `ddcutil` itself is built on. That's the hard, error-prone part
//! (capability-string parsing, per-version quirks); there's no reason to
//! re-derive it by hand here.
//!
//! Enumeration is our own (`/dev/i2c-*`, probed with a live `getvcp 0xdf`)
//! rather than `ddc-i2c`'s built-in `with-linux-enumerate`, so this backend
//! doesn't drag in `libudev`/`pkg-config` as a build-time system dependency.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use ddc::{Ddc, Edid};
use ddc_i2c::I2cDeviceDdc;
use mccs_db::{Access, Database, Descriptor, ValueInterpretation, ValueType as DbValueType};

use crate::vcp::{Capabilities, Display, FeatureReading, RawBytes, VcpFeature, VcpValue};

use super::{BackendError, DdcBackend, Result};

/// One open handle plus whatever we've learned about its feature set so
/// far. `raw`/`db` are populated lazily on first use (by `capabilities()`
/// or `get_vcp()`, whichever comes first) rather than during `detect()`,
/// same as the `ddcutil` backend only reads capabilities when something
/// actually asks for them.
struct Entry {
    path: PathBuf,
    handle: I2cDeviceDdc,
    mfg_id: String,
    model: String,
    vcp_version: String,
    /// Same version as `vcp_version`, kept parsed rather than just
    /// formatted — this is what actually selects the MCCS feature
    /// database (see `ensure_caps`). It comes from a live `getvcp 0xdf`
    /// reply, which is more reliably present than the capabilities
    /// string's own (often-omitted) `mccs_ver` tag.
    vcp_version_parsed: mccs::Version,
    raw: Option<mccs::Capabilities>,
    db: Option<Database>,
}

/// `DdcBackend` implementation that talks I2C directly — no `ddcutil`
/// subprocess involved.
///
/// Handles are kept open across calls (behind a `Mutex`, since
/// `DdcBackend` requires `Sync` but every `Ddc` operation needs `&mut`)
/// rather than reopened per call: a DDC/CI round trip already burns tens of
/// milliseconds in mandated protocol delays, so reopening the device node
/// and re-probing it on every single slider tick would be wasteful.
pub struct NativeBackend {
    entries: Mutex<Vec<Entry>>,
}

impl NativeBackend {
    pub fn new() -> Self {
        NativeBackend {
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DdcBackend for NativeBackend {
    fn detect(&self) -> Result<Vec<Display>> {
        detect(&self.entries)
    }

    fn capabilities(&self, display_num: i32) -> Result<Capabilities> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entry_mut(&mut entries, display_num)?;
        ensure_caps(entry)?;
        Ok(to_capabilities(
            entry.raw.as_ref().unwrap(),
            entry.db.as_ref().unwrap(),
        ))
    }

    fn get_vcp(&self, display_num: i32, code: u8) -> Result<FeatureReading> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entry_mut(&mut entries, display_num)?;
        get_vcp(entry, code)
    }

    fn set_vcp(&self, display_num: i32, code: u8, value: u16, permit_unknown: bool) -> Result<()> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entry_mut(&mut entries, display_num)?;
        set_vcp(entry, code, value, permit_unknown)
    }
}

fn entry_mut(entries: &mut [Entry], display_num: i32) -> Result<&mut Entry> {
    entries
        .get_mut(display_num as usize - 1)
        .ok_or_else(|| BackendError::msg(format!("no such display: {display_num}")))
}

// ---- detect ----------------------------------------------------------

/// Scans `/dev/i2c-*` and keeps every bus that gives a live DDC/CI reply to
/// `getvcp 0xdf` (VCP version) — the cheapest way to confirm a monitor is
/// actually listening on the DDC/CI address, rather than some unrelated
/// i2c device (GPU internals, SMBus controllers, ...).
fn detect(slot: &Mutex<Vec<Entry>>) -> Result<Vec<Display>> {
    let mut dir_entries: Vec<_> = fs::read_dir("/dev")?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("i2c-"))
        .collect();
    dir_entries.sort_by_key(|e| e.file_name());

    let mut found = Vec::new();
    for dir_entry in dir_entries {
        let path = dir_entry.path();
        let Ok(mut handle) = ddc_i2c::from_i2c_device(&path) else {
            continue;
        };
        let Ok(version) = handle.get_vcp_feature(0xdf) else {
            continue;
        };

        let mut mfg_id = String::new();
        let mut model = String::new();
        let mut edid_buf = vec![0u8; 0x80];
        if handle.read_edid(0, &mut edid_buf).is_ok() {
            if let Ok(edid) = edid::parse(&edid_buf).to_result() {
                mfg_id = edid.header.vendor.iter().collect();
                for desc in &edid.descriptors {
                    if let edid::Descriptor::ProductName(name) = desc {
                        model = name.clone();
                    }
                }
            }
        }

        let parsed_version = mccs::Version::new(version.sh, version.sl);
        found.push(Entry {
            path,
            handle,
            mfg_id,
            model,
            vcp_version: parsed_version.to_string(),
            vcp_version_parsed: parsed_version,
            raw: None,
            db: None,
        });
    }

    let displays = found
        .iter()
        .enumerate()
        .map(|(i, e)| Display {
            number: (i + 1) as i32,
            bus: e.path.display().to_string(),
            // Not exposed by the plain i2c-dev path (no DRM connector
            // lookup here, unlike ddcutil) — left empty, same as the
            // `ddcutil` backend leaves it when the field is absent.
            connector: String::new(),
            mfg_id: e.mfg_id.clone(),
            model: e.model.clone(),
            vcp_version: e.vcp_version.clone(),
        })
        .collect();

    *slot.lock().unwrap() = found;
    Ok(displays)
}

// ---- capabilities ------------------------------------------------------

/// Populates `entry.raw`/`entry.db` on first use. A no-op once cached —
/// call again after a rescan (`detect()` replaces `Entry`s wholesale, so
/// there's nothing stale to invalidate here).
fn ensure_caps(entry: &mut Entry) -> Result<()> {
    if entry.db.is_some() {
        return Ok(());
    }

    let cap_bytes = entry
        .handle
        .capabilities_string()
        .map_err(|e| cap_err(&entry.path, &e))?;
    let raw = mccs_caps::parse_capabilities(&cap_bytes).map_err(|e| {
        BackendError::msg(format!("{}: capabilities: {e}", entry.path.display()))
    })?;

    // Prefer the version the capabilities string itself declares, but a
    // lot of monitors omit `mccs_ver` there even though they answer a
    // live `getvcp 0xdf` just fine (already read during `detect`) — fall
    // back to that rather than `Database::default()`, which has no
    // entries at all and would make every single feature come back
    // unrecognized, not just the ones this monitor doesn't declare.
    let version = raw.mccs_version.unwrap_or(entry.vcp_version_parsed);
    let mut db = Database::from_version(&version);
    db.apply_capabilities(&raw);

    entry.raw = Some(raw);
    entry.db = Some(db);
    Ok(())
}

fn to_capabilities(raw: &mccs::Capabilities, db: &Database) -> Capabilities {
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
        let values = cap_desc
            .values
            .iter()
            .map(|(&v_code, v_name)| VcpValue {
                code: v_code,
                name: v_name.clone().unwrap_or_default(),
            })
            .collect();

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
/// database. Non-continuous entries carry no value labels here — those
/// still come from whatever the monitor's own capability string declares
/// (see `to_capabilities`), same as for codes the real database does
/// recognize.
const WELL_KNOWN: &[(u8, &str, bool)] = &[
    // (code, name, is_continuous)
    (0x10, "Brightness", true),
    (0x12, "Contrast", true),
    (0x14, "Select Color Preset", false),
    (0x16, "Video Gain (Red)", true),
    (0x18, "Video Gain (Green)", true),
    (0x1a, "Video Gain (Blue)", true),
    (0x60, "Input Source", false),
    (0x62, "Audio Speaker Volume", true),
    (0x8d, "Audio Mute", false),
    (0xd6, "Power Mode", false),
];

fn well_known(code: u8) -> Option<Descriptor> {
    WELL_KNOWN
        .iter()
        .find(|&&(c, _, _)| c == code)
        .map(|&(code, name, continuous)| Descriptor {
            name: Some(name.to_string()),
            code,
            ty: if continuous {
                DbValueType::Continuous {
                    interpretation: ValueInterpretation::Continuous,
                }
            } else {
                DbValueType::NonContinuous {
                    values: Default::default(),
                    interpretation: ValueInterpretation::NonContinuous,
                }
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
/// capability string itself never carries human names. So a plain
/// `db.get(code).or_else(well_known)` would never reach the fallback: the
/// `Some(..)` from the capability-derived entry (already correctly typed
/// continuous-vs-non-continuous, going by whether it declares enum values)
/// wins every time, just nameless. Patch the name in from `well_known`
/// instead of replacing the whole descriptor.
fn resolve(db: &Database, code: u8) -> Option<Descriptor> {
    match db.get(code).cloned() {
        Some(d) if d.name.is_some() => Some(d),
        Some(mut d) => {
            d.name = well_known(code).and_then(|wk| wk.name);
            Some(d)
        }
        None => well_known(code),
    }
}

/// `(name, recognized)` for a feature code — `recognized` means we know
/// what the code means (spec database or `well_known` fallback), not just
/// that this monitor's capability string mentions it, same distinction
/// `ddcutil` draws.
fn describe(db: &Database, code: u8) -> (Option<String>, bool) {
    match resolve(db, code).and_then(|d| d.name) {
        Some(name) => (Some(name), true),
        None => (None, false),
    }
}

// ---- getvcp -------------------------------------------------------------

fn get_vcp(entry: &mut Entry, code: u8) -> Result<FeatureReading> {
    ensure_caps(entry)?;
    let descriptor = resolve(entry.db.as_ref().unwrap(), code);

    if let Some(d) = &descriptor {
        if d.access == Access::WriteOnly {
            return Ok(FeatureReading {
                code,
                name: d.name.clone().unwrap_or_default(),
                readable: false,
                ..Default::default()
            });
        }
    }

    let value = entry
        .handle
        .get_vcp_feature(code)
        .map_err(|e| BackendError::msg(format!("{}: getvcp {code:02x}: {e}", entry.path.display())))?;
    let name = descriptor.as_ref().and_then(|d| d.name.clone()).unwrap_or_default();

    Ok(match descriptor.map(|d| d.ty) {
        Some(DbValueType::Continuous { .. }) => FeatureReading {
            code,
            name,
            readable: true,
            continuous: true,
            current: value.value(),
            max: value.maximum(),
            ..Default::default()
        },
        Some(DbValueType::NonContinuous { values, .. }) => FeatureReading {
            code,
            name,
            readable: true,
            current: value.sl as u16,
            label: values.get(&value.sl).cloned().flatten().unwrap_or_default(),
            ..Default::default()
        },
        // Table type and "unknown to the spec" both land here: neither has
        // a scalar reading to report, so keep the raw reply bytes exactly
        // like the `ddcutil` backend does for its unrecognized codes.
        _ => FeatureReading {
            code,
            name,
            readable: true,
            current: value.sl as u16,
            raw: Some(RawBytes {
                mh: value.mh,
                ml: value.ml,
                sh: value.sh,
                sl: value.sl,
            }),
            ..Default::default()
        },
    })
}

// ---- setvcp -------------------------------------------------------------

fn set_vcp(entry: &mut Entry, code: u8, value: u16, permit_unknown: bool) -> Result<()> {
    if !permit_unknown {
        // Best-effort safety gate mirroring ddcutil's
        // --permit-unknown-feature: if we already know this code isn't in
        // the spec DB, refuse rather than poke an undocumented register.
        // If capabilities haven't been read yet, skip the check rather
        // than force an extra round trip just to enforce it.
        if let Some(db) = &entry.db {
            if !describe(db, code).1 {
                return Err(BackendError::msg(format!(
                    "{}: refusing to set unrecognized VCP code {code:02x} without permit_unknown",
                    entry.path.display()
                )));
            }
        }
    }

    entry
        .handle
        .set_vcp_feature(code, value)
        .map_err(|e| BackendError::msg(format!("{}: setvcp {code:02x} {value}: {e}", entry.path.display())))?;

    // Read the value back to confirm the monitor actually changed, same
    // contract the `ddcutil` backend upholds (see `DdcBackend::set_vcp`'s
    // docs) — some monitors accept a write and silently clamp/ignore it.
    let readback = entry.handle.get_vcp_feature(code).map_err(|e| {
        BackendError::msg(format!(
            "{}: setvcp {code:02x} {value}: verify read failed: {e}",
            entry.path.display()
        ))
    })?;
    if readback.value() != value {
        return Err(BackendError::msg(format!(
            "{}: setvcp {code:02x} {value}: monitor reports {} after write",
            entry.path.display(),
            readback.value()
        )));
    }

    Ok(())
}

fn cap_err(path: &std::path::Path, e: &impl std::fmt::Display) -> BackendError {
    BackendError::msg(format!("{}: capabilities: {e}", path.display()))
}

#[cfg(test)]
mod live_probe {
    use super::*;

    #[test]
    #[ignore]
    fn manual_detect_probe() {
        let backend = NativeBackend::new();
        let displays = backend.detect().expect("detect failed");
        for d in &displays {
            eprintln!("{d:?}");
        }
        for d in &displays {
            let caps = backend.capabilities(d.number).expect("capabilities failed");
            eprintln!("caps: model={:?} mccs={:?} features={}", caps.model, caps.mccs_version, caps.features.len());
            if let Some(f) = caps.features.iter().find(|f| f.code == 0x10) {
                let reading = backend.get_vcp(d.number, f.code).expect("get_vcp 0x10 failed");
                eprintln!("brightness reading: {reading:?}");
            }
        }
    }
}
