//! On-disk, versioned cache of a monitor's scan result — see the "Caching,
//! and loading progressively" section of the original Go project's README
//! for the rationale. A straight port of `internal/ddc/cache.go`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::vcp::Capabilities;

/// Guards against silently misreading an older cache whose schema doesn't
/// match — serde ignores unknown/missing fields by default (with `#[serde
/// (default)]`), so a renamed field would otherwise load "successfully"
/// with data quietly missing instead of falling back to a fresh scan. Bump
/// this whenever `MonitorCache`'s shape changes.
const CACHE_VERSION: u32 = 1;

/// Enough to skip rediscovering a continuous feature: its code, max value,
/// and last known current value. `value` is what's shown immediately on
/// launch, before a fresh read confirms (or corrects) it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSlider {
    pub code: u8,
    pub max: u16,
    pub value: u16,
}

/// A non-continuous feature's code and last known selection, shown
/// immediately for the same reason as `CachedSlider::value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSelector {
    pub code: u8,
    pub selected: u8,
}

/// What gets persisted after the first full scan of a monitor: its
/// capabilities (essentially static — only changes on a firmware update),
/// which codes turned out to actually behave like a slider, selector, or
/// action once probed live (capabilities alone can't tell us that), and
/// each control's last known value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorCache {
    pub version: u32,
    pub capabilities: Capabilities,
    #[serde(default)]
    pub sliders: Vec<CachedSlider>,
    #[serde(default)]
    pub selectors: Vec<CachedSelector>,
    /// Actions are write-only, non-continuous features confirmed (by a live
    /// "is not readable" reply) to be commands rather than data — e.g.
    /// Restore factory defaults. They have no current value to refresh, so
    /// unlike sliders/selectors they need no live re-probe once known.
    #[serde(default)]
    pub action_codes: Vec<u8>,
}

fn cache_dir() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("vcpctl");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Identifies a monitor by manufacturer + model, which is what discovery
/// gives us from the EDID. Two monitors of the same model sharing a cache
/// entry is harmless — they have identical capabilities by definition.
fn cache_key(mfg_id: &str, model: &str) -> String {
    let s: String = format!("{mfg_id}-{model}")
        .chars()
        .map(|c| match c {
            ' ' | '/' | '\\' => '_',
            c => c,
        })
        .collect();
    if s.is_empty() || s == "-" {
        "unknown".to_string()
    } else {
        s
    }
}

fn cache_path(mfg_id: &str, model: &str) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("monitor-{}.json", cache_key(mfg_id, model))))
}

/// Returns the cached scan for a monitor, or `None` if there isn't one, it
/// can't be read/parsed, or it was written by an incompatible version —
/// all treated the same as a miss, so anything short of a clean match just
/// triggers a fresh scan.
pub fn load(mfg_id: &str, model: &str) -> Option<MonitorCache> {
    let path = cache_path(mfg_id, model)?;
    let data = std::fs::read_to_string(path).ok()?;
    let cache: MonitorCache = serde_json::from_str(&data).ok()?;
    if cache.version != CACHE_VERSION {
        return None;
    }
    Some(cache)
}

/// Persists a scan. Failure is not fatal to the caller — worst case, the
/// next launch just scans again.
pub fn save(mfg_id: &str, model: &str, mut cache: MonitorCache) -> std::io::Result<()> {
    let path = cache_path(mfg_id, model)
        .ok_or_else(|| std::io::Error::other("no cache directory available"))?;
    cache.version = CACHE_VERSION;
    let data = serde_json::to_string_pretty(&cache)?;
    std::fs::write(path, data)
}

/// Removes a cached scan, forcing the next load to rediscover everything
/// from scratch (used by the manual "rescan" action).
pub fn clear(mfg_id: &str, model: &str) -> std::io::Result<()> {
    let Some(path) = cache_path(mfg_id, model) else {
        return Ok(());
    };
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcp::VcpFeature;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    // `XDG_CACHE_HOME` is process-global, and cargo runs tests in the same
    // process across threads — this lock serializes every test that
    // touches it (the Go original gets this for free from `t.Setenv`,
    // which auto-serializes affected tests; there's no such thing built
    // into `cargo test`).
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Points `XDG_CACHE_HOME` at a fresh temp directory for the duration
    /// of `f`, mirroring the Go tests' `t.Setenv("XDG_CACHE_HOME",
    /// t.TempDir())`.
    fn with_temp_cache_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("vcpctl-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp cache dir");

        // SAFETY: serialized by ENV_LOCK above — no other thread in this
        // process reads/writes the environment while this guard is held.
        unsafe { std::env::set_var("XDG_CACHE_HOME", &dir) };

        let result = f();

        unsafe { std::env::remove_var("XDG_CACHE_HOME") };
        let _ = std::fs::remove_dir_all(&dir);

        result
    }

    fn sample_cache() -> MonitorCache {
        MonitorCache {
            version: 0, // stamped by `save`
            capabilities: Capabilities {
                model: "Not specified".to_string(),
                mccs_version: "2.1".to_string(),
                features: vec![VcpFeature {
                    code: 0x10,
                    name: "Brightness".to_string(),
                    recognized: true,
                    ..Default::default()
                }],
            },
            sliders: vec![CachedSlider {
                code: 0x10,
                max: 100,
                value: 80,
            }],
            selectors: vec![
                CachedSelector {
                    code: 0x60,
                    selected: 0x11,
                },
                CachedSelector {
                    code: 0x14,
                    selected: 0x05,
                },
            ],
            action_codes: Vec::new(),
        }
    }

    #[test]
    fn monitor_cache_round_trip() {
        with_temp_cache_home(|| {
            let want = sample_cache();
            save("GSM", "LG ULTRAWIDE", want.clone()).expect("save");

            let got = load("GSM", "LG ULTRAWIDE").expect("expected a hit after saving");
            assert_eq!(got.capabilities.mccs_version, want.capabilities.mccs_version);
            assert_eq!(got.sliders.len(), 1);
            assert_eq!(got.sliders[0].code, 0x10);
            assert_eq!(got.sliders[0].max, 100);
            assert_eq!(got.sliders[0].value, 80);
            assert_eq!(got.selectors.len(), 2);
            assert_eq!(got.selectors[0].selected, 0x11);
        });
    }

    #[test]
    fn monitor_cache_version_mismatch_is_treated_as_miss() {
        with_temp_cache_home(|| {
            // `save` always stamps the current version, so write the file
            // directly to actually simulate a stale one.
            let path = cache_path("GSM", "LG ULTRAWIDE").unwrap();
            let stale = MonitorCache {
                version: CACHE_VERSION - 1,
                ..sample_cache()
            };
            std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();

            assert!(
                load("GSM", "LG ULTRAWIDE").is_none(),
                "expected a version mismatch to be treated as a cache miss"
            );
        });
    }

    #[test]
    fn monitor_cache_miss_when_not_saved() {
        with_temp_cache_home(|| {
            assert!(
                load("NoOne", "Never Saved").is_none(),
                "expected a miss for a monitor that was never cached"
            );
        });
    }

    #[test]
    fn monitor_cache_clear_forces_miss() {
        with_temp_cache_home(|| {
            save("GSM", "LG ULTRAWIDE", MonitorCache::default()).unwrap();
            assert!(
                load("GSM", "LG ULTRAWIDE").is_some(),
                "setup: expected a hit before clearing"
            );

            clear("GSM", "LG ULTRAWIDE").expect("clear");
            assert!(
                load("GSM", "LG ULTRAWIDE").is_none(),
                "expected a miss after clearing the cache"
            );
        });
    }

    #[test]
    fn cache_key_sanitizes_separators() {
        assert!(!cache_key("GSM", "LG/ULTRAWIDE 29\"").is_empty());
        // Must not contain path separators that would escape the cache dir.
        assert_eq!(cache_key("A/B", "C\\D"), "A_B-C_D");
    }
}
