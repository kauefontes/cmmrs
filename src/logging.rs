//! A minimal file-backed logger — implements `log::Log` directly rather
//! than pulling in a logging backend crate (`env_logger`, `simplelog`,
//! `fern`, ...). Every one of those defaults to writing to stderr, and
//! this app spends its entire life with the terminal in raw mode on the
//! alternate screen: stderr output during that time is invisible at best
//! and garbles the display at worst (see the `eprintln!`s this replaces
//! in `main::pick_backend`, which were doing exactly that). Logging here
//! always means "to a file", so hand-rolling the ~50 lines that needs is
//! simpler than fighting a crate's stderr-first defaults.
//!
//! Level is controlled by `RUST_LOG` — `error`/`warn`/`info`/`debug`/
//! `trace`, case-insensitive, same names `env_logger` uses, just without
//! its per-module directive syntax (`RUST_LOG=my_crate=debug` and the
//! like aren't supported; it's one global level). Defaults to `info`.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

/// Ceiling on the log file's size — past it, the file is dropped and
/// started fresh rather than left to grow forever. This is a debugging
/// aid, not an audit trail, so losing old entries on rotation is fine;
/// there's no `.log.1` backup.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

struct FileLogger {
    file: Mutex<std::fs::File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // A poisoned mutex (a previous write panicked mid-lock) still
        // holds a perfectly usable `File` — logging shouldn't be the
        // thing that takes the app down, so recover it rather than
        // propagate the panic.
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(
            file,
            "{} {:<5} {}: {}",
            now_iso8601(),
            record.level(),
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = file.flush();
    }
}

/// Installs the global logger. Best-effort and infallible by design: if
/// there's no cache dir, the filesystem is read-only, whatever — logging
/// just silently becomes a no-op (the `log` macros already compile down
/// to nothing when no logger is installed) rather than a startup failure.
/// Nothing in this app depends on the log file existing.
pub fn init() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::Info);

    let Some(path) = crate::cache::log_path() else {
        return;
    };
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    let logger = FileLogger {
        file: Mutex::new(file),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
        log::info!(
            "cmmrs v{} starting (log level {level}, log file {})",
            env!("CARGO_PKG_VERSION"),
            path.display()
        );
    }
}

/// `SystemTime` -> `"YYYY-MM-DDTHH:MM:SSZ"`, hand-rolled to avoid a date/
/// time dependency just for log timestamps. The calendar math is Howard
/// Hinnant's civil-from-days algorithm — see
/// <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>.
fn now_iso8601() -> String {
    let total_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (total_secs / 86_400) as i64;
    let secs_of_day = total_secs % 86_400;
    let (hour, minute, second) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_values() {
        // Unix epoch itself.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2023-11-14, a commonly-cited reference point for 1_700_000_000.
        assert_eq!(civil_from_days(1_700_000_000 / 86_400), (2023, 11, 14));
        // A leap-year boundary: day before and the leap day itself.
        assert_eq!(civil_from_days(19_781), (2024, 2, 28));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
    }
}
