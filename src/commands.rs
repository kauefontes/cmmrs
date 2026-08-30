//! Async work: everything that talks to a `DdcBackend` runs as a job on
//! the single `Worker` thread and reports back through an
//! `mpsc::Sender<Msg>` — this is the same shape as the Go original's
//! `tea.Cmd`/`tea.Msg` pattern (bubbletea), just built by hand instead of
//! provided by a framework. See `main.rs`'s event loop for the receiving
//! side.
//!
//! Every job runs on `Worker`'s one thread, never a fresh one per call —
//! DDC/CI over i2c is a one-transaction-at-a-time bus, and firing off a
//! bare `std::thread::spawn` per backend call (the original shape here)
//! let a cache-hit probe fan out a dozen-plus concurrent `ddcutil getvcp`
//! calls that fought over the same i2c device's flock. See `worker`'s
//! module docs for what that did to real hardware.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::backend::{DdcBackend, Result as BackendResult};
use crate::cache::{self, CachedSelector, CachedSlider, MonitorCache};
use crate::components::{Action, Selector, SelectorOption, Slider};
use crate::vcp::{Capabilities, Display, FeatureReading};
use crate::worker::Worker;

/// Distinguishes the three kinds of control a probe result can surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlKind {
    Slider,
    Selector,
    Action,
}

/// Points at one entry in `sliders`/`selectors`/`actions`, in the order
/// they should be interleaved for navigation/rendering (the order their
/// VCP codes appear in capabilities).
#[derive(Debug, Clone, Copy)]
pub struct CtrlRef {
    pub kind: CtrlKind,
    pub idx: usize,
}

/// The friendly controls built from a probe, plus the capabilities they
/// were built from.
pub struct ProbeOk {
    pub caps: Capabilities,
    pub sliders: Vec<Slider>,
    pub selectors: Vec<Selector>,
    pub actions: Vec<Action>,
    pub order: Vec<CtrlRef>,
}

pub enum Msg {
    Detect(BackendResult<Vec<Display>>),
    Probe(BackendResult<ProbeOk>),
    /// The immediate render built purely from a cached scan — no i2c
    /// round-trip yet, just whatever value was last known. Followed by a
    /// batch of `LiveValue` messages that each confirm (or correct) one
    /// control's value shortly after.
    CachedControls(ProbeOk),
    LiveValue {
        code: u8,
        result: BackendResult<FeatureReading>,
    },
    Set {
        code: u8,
        value: u16,
        result: BackendResult<()>,
    },
    ActionDone {
        result: BackendResult<()>,
    },
    RawProbe(BackendResult<Vec<FeatureReading>>),
    /// A fresh reading for exactly one code, used to refresh a single row
    /// after a write from the Raw VCP edit flow instead of re-scanning
    /// everything.
    RawSingleProbe {
        code: u8,
        result: BackendResult<FeatureReading>,
    },
    RawSet {
        code: u8,
        result: BackendResult<()>,
    },
}

/// A deferred unit of I/O — the Rust analogue of `tea.Cmd`. `App`'s
/// update logic never touches a `DdcBackend` or a channel directly; it
/// only ever *describes* what should happen next by returning a `Cmd`
/// (see `app::App::handle_key`/`handle_msg`). `dispatch` is what actually
/// turns one into a running thread. That split is what makes `App`
/// testable without a real backend or any actual I/O — same reason the Go
/// original's `Model.Update` only ever returns a `tea.Cmd` value instead
/// of running the command itself.
pub enum Cmd {
    Detect,
    Probe(Display),
    Set { display_num: i32, code: u8, value: u16 },
    Action { display_num: i32, code: u8 },
    RawProbe { display_num: i32, codes: Vec<u8> },
    RawSingleProbe { display_num: i32, code: u8 },
    RawSet { display_num: i32, code: u8, value: u16, permit_unknown: bool },
}

/// Actually runs a `Cmd`: queues whatever job(s) it takes to perform the
/// I/O onto `worker`, each reporting back through `tx`. Called only from
/// `main`'s event loop, never from `App` or from tests. Queuing (not a
/// fresh thread per call) is what keeps every backend call serialized —
/// see `worker`'s module docs for why that matters here.
pub fn dispatch(cmd: Cmd, backend: &Arc<dyn DdcBackend>, tx: &Sender<Msg>, worker: &Worker) {
    match cmd {
        Cmd::Detect => spawn_detect(backend.clone(), tx.clone(), worker),
        Cmd::Probe(display) => spawn_probe(backend.clone(), tx.clone(), display, worker),
        Cmd::Set { display_num, code, value } => {
            spawn_set(backend.clone(), tx.clone(), display_num, code, value, worker)
        }
        Cmd::Action { display_num, code } => {
            spawn_action(backend.clone(), tx.clone(), display_num, code, worker)
        }
        Cmd::RawProbe { display_num, codes } => {
            spawn_raw_probe(backend.clone(), tx.clone(), display_num, codes, worker)
        }
        Cmd::RawSingleProbe { display_num, code } => {
            spawn_raw_single_probe(backend.clone(), tx.clone(), display_num, code, worker)
        }
        Cmd::RawSet { display_num, code, value, permit_unknown } => {
            spawn_raw_set(backend.clone(), tx.clone(), display_num, code, value, permit_unknown, worker)
        }
    }
}

fn spawn_detect(backend: Arc<dyn DdcBackend>, tx: Sender<Msg>, worker: &Worker) {
    worker.submit(Box::new(move || {
        let result = backend.detect();
        match &result {
            Ok(displays) => log::info!("detect: found {} display(s)", displays.len()),
            Err(e) => log::warn!("detect failed: {e}"),
        }
        let _ = tx.send(Msg::Detect(result));
    }));
}

/// Discovers a display's controls, preferring a cached scan.
///
/// A full scan means one `getvcp` per feature, which can be a slow
/// per-code round trip depending on the backend — most of those codes
/// never become a control (see `build_controls`), so once a monitor's been
/// scanned once, only the codes that did are re-read live next time.
/// Actions (write-only, nothing to read) don't need re-probing at all once
/// known.
///
/// On a cache hit, the screen doesn't wait for those live reads either: a
/// `CachedControls` message is sent immediately with each control's last
/// known value, then each one is confirmed independently as its own
/// `LiveValue` message arrives, rather than blocking on one big scan.
fn spawn_probe(backend: Arc<dyn DdcBackend>, tx: Sender<Msg>, display: Display, worker: &Worker) {
    if let Some(cache) = cache::load(&display.mfg_id, &display.model) {
        let (sliders, selectors, actions, order) = build_controls_from_cache(&cache);
        let live_codes: Vec<u8> = cache
            .sliders
            .iter()
            .map(|s| s.code)
            .chain(cache.selectors.iter().map(|s| s.code))
            .collect();

        let caps = cache.capabilities.clone();
        let _ = tx.send(Msg::CachedControls(ProbeOk {
            caps,
            sliders,
            selectors,
            actions,
            order,
        }));

        // Queued, not fan-out spawned: each code's read still arrives as
        // its own `LiveValue` message (so the screen fills in
        // independently, control by control), but the worker runs them
        // one at a time instead of all at once — see `worker`'s module
        // docs for why concurrent DDC reads here previously froze the
        // whole machine.
        for code in live_codes {
            spawn_live_value(backend.clone(), tx.clone(), display.number, code, worker);
        }
        return;
    }

    worker.submit(Box::new(move || {
        let result = (|| -> BackendResult<ProbeOk> {
            let caps = backend.capabilities(display.number).inspect_err(|e| {
                log::warn!("probe: capabilities for display {}: {e}", display.number);
            })?;
            log::debug!(
                "probe: display {} declared {} feature(s), MCCS {}",
                display.number,
                caps.features.len(),
                caps.mccs_version
            );
            // Only recognized features can ever become a control (see
            // build_controls) — no point spending a round-trip on the
            // unrecognized/manufacturer-specific codes just to discover
            // that. Those stay unscanned until the Raw VCP screen asks.
            let codes: Vec<u8> = caps
                .features
                .iter()
                .filter(|f| f.recognized)
                .map(|f| f.code)
                .collect();

            let mut readings = Vec::with_capacity(codes.len());
            for code in codes {
                match backend.get_vcp(display.number, code) {
                    Ok(r) => readings.push(r),
                    // Not fatal to the whole scan — one flaky code just
                    // means one fewer control shows up this launch — but
                    // silently dropping it left exactly this kind of gap
                    // invisible, so it's worth a line.
                    Err(e) => log::warn!("probe: getvcp {code:#04x} on display {}: {e}", display.number),
                }
            }

            let (sliders, selectors, actions, order) = build_controls(&caps, &readings, None);
            log::info!(
                "probe: display {} → {} slider(s), {} selector(s), {} action(s) ({} unrecognized/mfg-specific)",
                display.number,
                sliders.len(),
                selectors.len(),
                actions.len(),
                caps.features.iter().filter(|f| !f.recognized).count(),
            );

            // Best-effort: a failed save just means the next launch scans
            // again.
            let _ = cache::save(
                &display.mfg_id,
                &display.model,
                to_monitor_cache(&caps, &sliders, &selectors, &actions),
            );

            Ok(ProbeOk {
                caps,
                sliders,
                selectors,
                actions,
                order,
            })
        })();
        let _ = tx.send(Msg::Probe(result));
    }));
}

/// Reads one VCP code's live value, independent of any other code — this
/// is what lets a cache-hit launch report N reads as they each complete
/// instead of blocking behind a single loading screen. They still run one
/// at a time on `worker`, though — only the *reporting* is per-code, not
/// the bus access.
fn spawn_live_value(backend: Arc<dyn DdcBackend>, tx: Sender<Msg>, display_num: i32, code: u8, worker: &Worker) {
    worker.submit(Box::new(move || {
        let result = backend.get_vcp(display_num, code);
        if let Err(e) = &result {
            log::warn!("live value: getvcp {code:#04x} on display {display_num}: {e}");
        }
        let _ = tx.send(Msg::LiveValue { code, result });
    }));
}

fn spawn_set(backend: Arc<dyn DdcBackend>, tx: Sender<Msg>, display_num: i32, code: u8, value: u16, worker: &Worker) {
    worker.submit(Box::new(move || {
        let result = backend.set_vcp(display_num, code, value, false);
        if let Err(e) = &result {
            log::warn!("setvcp {code:#04x}={value} on display {display_num}: {e}");
        }
        let _ = tx.send(Msg::Set { code, value, result });
    }));
}

/// Triggers a write-only action feature (e.g. Restore factory defaults).
/// Per MCCS convention for Write-Only Non-Continuous features, the value
/// written doesn't carry data — it just needs to be non-zero to fire the
/// command, so this always sends 1.
fn spawn_action(backend: Arc<dyn DdcBackend>, tx: Sender<Msg>, display_num: i32, code: u8, worker: &Worker) {
    worker.submit(Box::new(move || {
        let result = backend.set_vcp(display_num, code, 1, false);
        match &result {
            Ok(()) => log::info!("action {code:#04x} fired on display {display_num}"),
            Err(e) => log::warn!("action {code:#04x} on display {display_num}: {e}"),
        }
        let _ = tx.send(Msg::ActionDone { result });
    }));
}

/// Reads every declared VCP code live, including the
/// unrecognized/manufacturer-specific ones the normal startup scan skips.
/// Deliberately only triggered when the user opens the Raw VCP screen.
fn spawn_raw_probe(backend: Arc<dyn DdcBackend>, tx: Sender<Msg>, display_num: i32, codes: Vec<u8>, worker: &Worker) {
    worker.submit(Box::new(move || {
        let mut readings = Vec::with_capacity(codes.len());
        for code in codes {
            match backend.get_vcp(display_num, code) {
                Ok(r) => readings.push(r),
                Err(e) => log::warn!("raw probe: getvcp {code:#04x} on display {display_num}: {e}"),
            }
        }
        let _ = tx.send(Msg::RawProbe(Ok(readings)));
    }));
}

fn spawn_raw_single_probe(backend: Arc<dyn DdcBackend>, tx: Sender<Msg>, display_num: i32, code: u8, worker: &Worker) {
    worker.submit(Box::new(move || {
        let result = backend.get_vcp(display_num, code);
        if let Err(e) = &result {
            log::debug!("raw single probe: getvcp {code:#04x} on display {display_num}: {e}");
        }
        let _ = tx.send(Msg::RawSingleProbe { code, result });
    }));
}

/// Writes `value` to `code`. Unrecognized/manufacturer-specific codes need
/// `permit_unknown`, which is exactly why the Raw VCP edit flow forces its
/// own confirmation prompt before ever reaching here.
fn spawn_raw_set(
    backend: Arc<dyn DdcBackend>,
    tx: Sender<Msg>,
    display_num: i32,
    code: u8,
    value: u16,
    permit_unknown: bool,
    worker: &Worker,
) {
    worker.submit(Box::new(move || {
        let result = backend.set_vcp(display_num, code, value, permit_unknown);
        if let Err(e) = &result {
            log::warn!("raw setvcp {code:#04x}={value} on display {display_num}: {e}");
        }
        let _ = tx.send(Msg::RawSet { code, result });
    }));
}

/// Rebuilds the friendly controls straight from a cached scan, with no
/// live reading involved — every code in it was already confirmed to
/// behave like a slider/selector/action on a previous full scan, so
/// there's nothing left to (re)classify, only values to trust
/// provisionally until a live read confirms them.
pub fn build_controls_from_cache(
    cache: &MonitorCache,
) -> (Vec<Slider>, Vec<Selector>, Vec<Action>, Vec<CtrlRef>) {
    let slider_by_code: HashMap<u8, &CachedSlider> =
        cache.sliders.iter().map(|s| (s.code, s)).collect();
    let selector_by_code: HashMap<u8, &CachedSelector> =
        cache.selectors.iter().map(|s| (s.code, s)).collect();
    let action_codes: HashSet<u8> = cache.action_codes.iter().copied().collect();

    let mut sliders = Vec::new();
    let mut selectors = Vec::new();
    let mut actions = Vec::new();
    let mut order = Vec::new();

    // Walk capabilities' own order so this matches what a fresh scan would
    // have produced, not cache-file insertion order.
    for f in &cache.capabilities.features {
        if let Some(cs) = slider_by_code.get(&f.code) {
            sliders.push(Slider::new(cs.code, f.name.clone(), cs.value, cs.max));
            order.push(CtrlRef {
                kind: CtrlKind::Slider,
                idx: sliders.len() - 1,
            });
            continue;
        }
        if let Some(cs) = selector_by_code.get(&f.code) {
            let opts = f
                .values
                .iter()
                .map(|v| SelectorOption {
                    code: v.code,
                    name: v.name.clone(),
                })
                .collect();
            selectors.push(Selector::new(cs.code, f.name.clone(), opts, cs.selected));
            order.push(CtrlRef {
                kind: CtrlKind::Selector,
                idx: selectors.len() - 1,
            });
            continue;
        }
        if action_codes.contains(&f.code) {
            actions.push(Action {
                code: f.code,
                name: f.name.clone(),
            });
            order.push(CtrlRef {
                kind: CtrlKind::Action,
                idx: actions.len() - 1,
            });
        }
    }
    (sliders, selectors, actions, order)
}

pub fn to_monitor_cache(
    caps: &Capabilities,
    sliders: &[Slider],
    selectors: &[Selector],
    actions: &[Action],
) -> MonitorCache {
    MonitorCache {
        version: 0, // stamped by cache::save
        capabilities: caps.clone(),
        sliders: sliders
            .iter()
            .map(|s| CachedSlider {
                code: s.code,
                max: s.max,
                value: s.value,
            })
            .collect(),
        selectors: selectors
            .iter()
            .map(|s| CachedSelector {
                code: s.code,
                selected: s.selected,
            })
            .collect(),
        action_codes: actions.iter().map(|a| a.code).collect(),
    }
}

/// Turns a probe result into the three kinds of friendly control the
/// controls screen understands:
///   - continuous features (Brightness, Contrast, RGB gain, Volume, ...) become sliders
///   - non-continuous features whose every declared value has a name
///     (Input Source, Color Preset, Power mode, ...) become selectors
///   - write-only features with nothing to read back (Restore factory
///     defaults, ...) become actions
///
/// Everything else — unrecognized codes, manufacturer-specific codes, and
/// enums with unnamed/"interpretation unavailable" values — is
/// deliberately left out. That's not data loss: it belongs on the Raw VCP
/// screen, not mixed into a "friendly" view that implies it's understood.
///
/// `known_action_codes` lets a code be classified as an action without a
/// live reading in hand — used when restoring from a cache that never
/// re-probes actions. Pass `None` when every recognized code was actually
/// probed this run.
pub fn build_controls(
    caps: &Capabilities,
    readings: &[FeatureReading],
    known_action_codes: Option<&HashSet<u8>>,
) -> (Vec<Slider>, Vec<Selector>, Vec<Action>, Vec<CtrlRef>) {
    let by_code: HashMap<u8, &FeatureReading> = readings.iter().map(|r| (r.code, r)).collect();

    let mut sliders = Vec::new();
    let mut selectors = Vec::new();
    let mut actions = Vec::new();
    let mut order = Vec::new();

    let add_action = |f: &crate::vcp::VcpFeature,
                       actions: &mut Vec<Action>,
                       order: &mut Vec<CtrlRef>| {
        actions.push(Action {
            code: f.code,
            name: f.name.clone(),
        });
        order.push(CtrlRef {
            kind: CtrlKind::Action,
            idx: actions.len() - 1,
        });
    };

    for f in &caps.features {
        if !f.recognized {
            continue;
        }

        let Some(r) = by_code.get(&f.code) else {
            if known_action_codes.map(|s| s.contains(&f.code)).unwrap_or(false) {
                add_action(f, &mut actions, &mut order);
            }
            continue;
        };

        if !r.readable {
            add_action(f, &mut actions, &mut order);
            continue;
        }

        if r.continuous {
            sliders.push(Slider::new(f.code, f.name.clone(), r.current, r.max));
            order.push(CtrlRef {
                kind: CtrlKind::Slider,
                idx: sliders.len() - 1,
            });
        } else if f.has_values() && f.all_values_named() {
            let opts = f
                .values
                .iter()
                .map(|v| SelectorOption {
                    code: v.code,
                    name: v.name.clone(),
                })
                .collect();
            selectors.push(Selector::new(f.code, f.name.clone(), opts, r.current as u8));
            order.push(CtrlRef {
                kind: CtrlKind::Selector,
                idx: selectors.len() - 1,
            });
        }
    }
    (sliders, selectors, actions, order)
}

pub fn all_feature_codes(caps: &Capabilities) -> Vec<u8> {
    caps.features.iter().map(|f| f.code).collect()
}
