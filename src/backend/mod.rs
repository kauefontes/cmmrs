//! The `DdcBackend` trait is the seam between `vcp`/`tui` and however a
//! monitor is actually talked to.
//!
//! `ddcutil` (see `backend::ddcutil`) was the only implementation at first,
//! same as the Go original — but nothing above this trait knows that.
//! `backend::native` (Linux, over `/dev/i2c-*` via `ddc`/`ddc-i2c`) and
//! `backend::macos` (macOS, over IOKit via `ddc-macos`) are native
//! backends; each is just another `impl DdcBackend`, selected at
//! runtime by `main::pick_backend`, with zero changes to `vcp` or `tui`.
//! Still to come: Windows (the Monitor Configuration Functions API). The
//! two native backends that exist share their capability-database logic
//! (name/type resolution, the `WELL_KNOWN` fallback table) via
//! `mccs_shared` rather than each carrying its own copy.

pub mod ddcutil;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod mccs_shared;
#[cfg(target_os = "linux")]
pub mod native;

use crate::vcp::{Capabilities, Display, FeatureReading};

pub type Result<T> = std::result::Result<T, BackendError>;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl BackendError {
    pub fn msg(s: impl Into<String>) -> Self {
        BackendError::Message(s.into())
    }
}

/// Everything the TUI needs from a DDC/CI backend. Every call is
/// synchronous and blocking by design — `tui`'s command layer is what runs
/// these off the render thread (see `commands.rs`), so a backend
/// implementation should stay simple and not worry about async itself.
pub trait DdcBackend: Send + Sync {
    /// Find every DDC/CI-capable display (laptop panels and other
    /// non-DDC displays excluded).
    fn detect(&self) -> Result<Vec<Display>>;

    /// Read and parse a display's declared VCP features.
    fn capabilities(&self, display_num: i32) -> Result<Capabilities>;

    /// Read one feature's live value. A write-only/action feature is not an
    /// error: it comes back with `readable: false`.
    fn get_vcp(&self, display_num: i32, code: u8) -> Result<FeatureReading>;

    /// Write a value to a feature. A backend that verifies the write by
    /// reading it back (as `ddcutil` does) should treat a mismatch as an
    /// error here — callers rely on `Ok(())` meaning the monitor actually
    /// changed, not just that a command was sent.
    ///
    /// `permit_unknown` mirrors ddcutil's `--permit-unknown-feature` safety
    /// gate for writing to codes the backend doesn't recognize; a backend
    /// with no such concept can ignore it.
    fn set_vcp(&self, display_num: i32, code: u8, value: u16, permit_unknown: bool) -> Result<()>;
}
