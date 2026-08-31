//! Native macOS DDC/CI backend: talks to monitors over IOKit via the
//! `ddc-macos` crate instead of shelling out to `ddcutil`. Structurally
//! the sibling of `backend::native` (same `Entry`-per-monitor shape,
//! same `mccs_shared` capability-database logic) — see that module's
//! docs for the parts that don't differ here.
//!
//! `ddc-macos` tries two transports itself, per monitor, in order: the
//! older `IOFramebuffer` port (Intel Macs) and, on Apple Silicon, the
//! `DCPAVServiceProxy`/`IOAVService` route — the same private-framework
//! mechanism [MonitorControl](https://github.com/MonitorControl/MonitorControl)
//! and `m1ddc` use for M-series DDC/CI, not something invented here.
//! Known, inherent limitation (not something this code can route
//! around): DDC/CI over some USB-C/Thunderbolt docks or cables just
//! doesn't get passed through on Apple Silicon — same caveat
//! MonitorControl documents.
//!
//! Unlike `backend::native`, EDID and the model name don't need manual
//! capability-string/EDID-descriptor parsing here for the model name —
//! `Monitor::product_name()` gets that from macOS directly. EDID bytes
//! (via `Monitor::edid()`) are still parsed with the same `edid` crate
//! `backend::native` uses, just for the vendor ID field, which
//! `ddc-macos` doesn't surface pre-parsed.

use std::sync::Mutex;

use ddc::Ddc;
use ddc_macos::Monitor;
use mccs_db::{Database, ValueType as DbValueType};

use crate::vcp::{Capabilities, Display, FeatureReading, RawBytes};

use super::mccs_shared::{describe, is_write_only, resolve, to_capabilities};
use super::{BackendError, DdcBackend, Result};

/// One enumerated monitor plus whatever we've learned about its feature
/// set so far — see `backend::native::Entry`, this is its macOS
/// counterpart (a `ddc_macos::Monitor` handle in place of an open
/// `/dev/i2c-*` file).
struct Entry {
    monitor: Monitor,
    mfg_id: String,
    model: String,
    vcp_version: String,
    /// See `backend::native::Entry`'s docs on the same field — same
    /// reasoning: prefer the version actually read live over whatever
    /// (often absent) `mccs_ver` tag the capability string declares.
    vcp_version_parsed: mccs::Version,
    raw: Option<mccs::Capabilities>,
    db: Option<Database>,
}

/// `DdcBackend` implementation over IOKit — no `ddcutil` subprocess
/// involved. See `backend::native::NativeBackend`'s docs for why handles
/// are kept open across calls instead of reopened per call.
pub struct MacosBackend {
    entries: Mutex<Vec<Entry>>,
}

impl MacosBackend {
    pub fn new() -> Self {
        MacosBackend {
            entries: Mutex::new(Vec::new()),
        }
    }
}

impl Default for MacosBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DdcBackend for MacosBackend {
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

/// Enumerates every online display `ddc_macos::Monitor::enumerate` finds
/// a working DDC service for (it already filters to those — see its own
/// docs) and confirms each actually replies to `getvcp 0xdf`, same
/// "prove it's really listening" check `backend::native::detect` makes.
fn detect(slot: &Mutex<Vec<Entry>>) -> Result<Vec<Display>> {
    let monitors = Monitor::enumerate().map_err(|e| BackendError::msg(format!("enumerate displays: {e}")))?;

    let mut found = Vec::new();
    for mut monitor in monitors {
        let version = match monitor.get_vcp_feature(0xdf) {
            Ok(v) => v,
            // Same reasoning as backend::native::detect: not every
            // enumerated display necessarily answers DDC/CI live (a
            // flaky cable/dock, one that only *looks* connected), so
            // this alone isn't warn-worthy.
            Err(e) => {
                log::debug!("macos detect: {} didn't answer getvcp 0xdf, skipping: {e}", monitor.description());
                continue;
            }
        };

        // `product_name()` comes from macOS's own parsed display info —
        // more reliable than re-deriving it from a raw EDID descriptor
        // the way backend::native has to. The EDID bytes are still
        // useful for the vendor ID, which isn't exposed pre-parsed.
        let model = monitor.product_name().unwrap_or_default();
        let mfg_id = monitor
            .edid()
            .and_then(|buf| edid::parse(&buf).to_result().ok())
            .map(|e| e.header.vendor.iter().collect())
            .unwrap_or_default();

        let parsed_version = mccs::Version::new(version.sh, version.sl);
        found.push(Entry {
            monitor,
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
            // No filesystem device path on macOS the way /dev/i2c-* is
            // on Linux — IOKit addresses displays by service, not path.
            bus: String::new(),
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

/// See `backend::native::ensure_caps` — identical logic, just reading
/// the capability string off `entry.monitor` instead of an i2c-dev
/// handle.
fn ensure_caps(entry: &mut Entry) -> Result<()> {
    if entry.db.is_some() {
        return Ok(());
    }

    let cap_bytes = entry
        .monitor
        .capabilities_string()
        .map_err(|e| BackendError::msg(format!("{}: capabilities: {e}", entry.monitor.description())))?;
    let raw = mccs_caps::parse_capabilities(&cap_bytes)
        .map_err(|e| BackendError::msg(format!("{}: capabilities: {e}", entry.monitor.description())))?;

    let version = raw.mccs_version.unwrap_or(entry.vcp_version_parsed);
    let mut db = Database::from_version(&version);
    db.apply_capabilities(&raw);

    entry.raw = Some(raw);
    entry.db = Some(db);
    Ok(())
}

// ---- getvcp -------------------------------------------------------------

/// See `backend::native::get_vcp` — identical logic and shared
/// `mccs_shared` resolution, just against `entry.monitor`.
fn get_vcp(entry: &mut Entry, code: u8) -> Result<FeatureReading> {
    ensure_caps(entry)?;
    let descriptor = resolve(entry.db.as_ref().unwrap(), code);

    if let Some(d) = &descriptor {
        if is_write_only(d) {
            return Ok(FeatureReading {
                code,
                name: d.name.clone().unwrap_or_default(),
                readable: false,
                ..Default::default()
            });
        }
    }

    let value = entry
        .monitor
        .get_vcp_feature(code)
        .map_err(|e| BackendError::msg(format!("{}: getvcp {code:02x}: {e}", entry.monitor.description())))?;
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

/// See `backend::native::set_vcp` — identical logic, just against
/// `entry.monitor`.
fn set_vcp(entry: &mut Entry, code: u8, value: u16, permit_unknown: bool) -> Result<()> {
    if !permit_unknown {
        if let Some(db) = &entry.db {
            if !describe(db, code).1 {
                return Err(BackendError::msg(format!(
                    "{}: refusing to set unrecognized VCP code {code:02x} without permit_unknown",
                    entry.monitor.description()
                )));
            }
        }
    }

    entry
        .monitor
        .set_vcp_feature(code, value)
        .map_err(|e| BackendError::msg(format!("{}: setvcp {code:02x} {value}: {e}", entry.monitor.description())))?;

    let readback = entry.monitor.get_vcp_feature(code).map_err(|e| {
        BackendError::msg(format!(
            "{}: setvcp {code:02x} {value}: verify read failed: {e}",
            entry.monitor.description()
        ))
    })?;
    if readback.value() != value {
        return Err(BackendError::msg(format!(
            "{}: setvcp {code:02x} {value}: monitor reports {} after write",
            entry.monitor.description(),
            readback.value()
        )));
    }

    Ok(())
}

/// Manual smoke test against real hardware — no macOS box in CI's
/// `macos-latest` runners has an actual external monitor attached, so
/// this needs a human. Mirrors
/// `backend::native::live_probe::manual_detect_probe`; run it with
/// `cargo test --release -- --ignored --nocapture manual_detect_probe`
/// on an M-series (or Intel) Mac with an external DDC/CI monitor
/// plugged in.
#[cfg(test)]
mod live_probe {
    use super::*;

    #[test]
    #[ignore]
    fn manual_detect_probe() {
        let backend = MacosBackend::new();
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
