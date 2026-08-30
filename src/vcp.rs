//! Backend-agnostic DDC/CI data model.
//!
//! Nothing in this module knows about `ddcutil`, subprocesses, or any
//! particular OS. It's the shared vocabulary every backend (subprocess
//! today, native i2c/OS APIs later — see `backend`) speaks, and the only
//! thing `tui` depends on. Swapping the backend should never require
//! touching this file or `tui`.

use serde::{Deserialize, Serialize};

/// One monitor found by a backend's discovery step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Display {
    /// Backend-assigned index used to address this display in later calls
    /// (ddcutil's "Display N", or a bus/handle id for a native backend).
    pub number: i32,
    /// Human-readable bus identifier, e.g. `/dev/i2c-2` on Linux. Purely
    /// informational — never parsed back.
    pub bus: String,
    /// DRM/OS connector name, e.g. `card1-HDMI-A-1`. May be empty.
    pub connector: String,
    pub mfg_id: String,
    pub model: String,
    pub vcp_version: String,
}

/// One enum value a feature can take, as declared by the monitor's
/// capabilities. `name` is empty when the monitor reports the code exists
/// but there's no known interpretation for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcpValue {
    pub code: u8,
    #[serde(default)]
    pub name: String,
}

/// Describes one VCP feature code exposed by the monitor.
///
/// `recognized`/`manufacturer_specific` reflect whether the backend could
/// identify the code itself, not whether we know how to *use* it — an
/// unrecognized or manufacturer-specific feature is still tracked and still
/// controllable from the Raw VCP screen, it just has no friendly
/// name/meaning yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VcpFeature {
    pub code: u8,
    pub name: String,
    pub recognized: bool,
    pub manufacturer_specific: bool,
    #[serde(default)]
    pub values: Vec<VcpValue>,
}

impl VcpFeature {
    /// Whether the monitor declared a fixed set of values for this feature
    /// (i.e. it's an enum, not a continuous 0..max control).
    pub fn has_values(&self) -> bool {
        !self.values.is_empty()
    }

    /// Not called by anything yet (same as the Go original, where it's
    /// exercised only by tests) — kept as query API for once the Raw VCP
    /// screen or a native backend needs to resolve a value's name outside
    /// of `Selector::current_name`.
    #[allow(dead_code)]
    pub fn value_name(&self, code: u8) -> Option<&str> {
        self.values
            .iter()
            .find(|v| v.code == code)
            .map(|v| v.name.as_str())
    }

    /// Whether every declared value has a name — only then does this
    /// feature become a friendly Selector; a partially-named enum stays on
    /// the Raw VCP screen instead of guessing.
    pub fn all_values_named(&self) -> bool {
        self.values.iter().all(|v| !v.name.is_empty())
    }
}

/// The parsed result of a capabilities query.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub model: String,
    pub mccs_version: String,
    pub features: Vec<VcpFeature>,
}

impl Capabilities {
    /// Same status as `VcpFeature::value_name` — unused outside tests so far.
    #[allow(dead_code)]
    pub fn feature(&self, code: u8) -> Option<&VcpFeature> {
        self.features.iter().find(|f| f.code == code)
    }
}

/// The four raw VCP reply bytes reported for a code with no known
/// interpretation (unrecognized or manufacturer-specific).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawBytes {
    pub mh: u8,
    pub ml: u8,
    pub sh: u8,
    pub sl: u8,
}

/// The result of reading one VCP feature's live value.
#[derive(Debug, Clone, Default)]
pub struct FeatureReading {
    pub code: u8,
    /// The backend's own name for the code, as reported by the live read.
    /// Not currently displayed anywhere — screens use the name declared in
    /// `Capabilities` instead — but kept rather than discarded per this
    /// project's rule that nothing a monitor reports gets silently
    /// dropped (same status in the Go original: parsed, never rendered).
    #[allow(dead_code)]
    pub name: String,
    /// false for write-only/action features ("is not readable").
    pub readable: bool,

    pub continuous: bool,
    pub current: u16,
    /// Only meaningful when `continuous`.
    pub max: u16,

    /// Parsed label for a known non-continuous value, e.g. "6500 K".
    pub label: String,
    /// Set only for the raw mh/ml/sh/sl form (unknown codes).
    pub raw: Option<RawBytes>,

    /// Marks a reading that came from a catch-all fallback format
    /// (frequencies, VCP version, firmware level, ...) rather than a shape
    /// with an actual value code attached. `current` is always its default
    /// here — it was never parsed, not genuinely 0 — so callers must not
    /// present `current` as a real value for these.
    pub generic: bool,
}
