# cmmrs

**c**ontrol-**m**y-**m**onitor, in **r**u**s**t. A terminal UI for
controlling DDC/CI monitors — any vendor, any monitor that speaks MCCS.
Rust port of a prior Go project (`lg-control-tui`) that was accidentally
named after the one monitor it was built against, even though nothing
about it was ever LG-specific — a mistake this name is deliberately built
to not repeat (it was originally called `vcpctl`, after the DDC/CI
protocol jargon "VCP", which is exactly as unfriendly as it sounds to
anyone who hasn't read the MCCS spec).

The point of this tool: everything a monitor exposes over DDC/CI —
brightness, contrast, RGB gain, input source, color presets, power mode,
speaker volume, whatever a given panel happens to have — controllable from
a keyboard-driven TUI (mouse too — see "Mouse" below), without diving into
an OSD menu with a joystick button to find it.

**Status: builds, 126 tests pass, and it's been run against real
hardware** (an LG ultrawide over native `/dev/i2c-2` — see "Porting
status"). Started as a file-by-file hand translation from the Go original;
the ratatui API surface details that translation had to guess at are now
verified against the compiler, not just written from memory. Grown well
past a straight port since — native-backend fixes, mouse support, a
visual redesign, file-backed logging — all covered by tests of their own.

## Architecture

```
src/
├── main.rs         — terminal setup/teardown, the event loop
├── app.rs           — application state + update logic (the Elm-style Model)
├── ui.rs             — top-level render dispatch, shared box chrome
├── commands.rs        — async work: describes/dispatches backend calls, reports back via mpsc
├── worker.rs           — single background thread that runs those calls one at a time
├── vcp.rs                — backend-agnostic data model (Display, Capabilities, VcpFeature, FeatureReading)
├── cache.rs               — on-disk, versioned scan cache, keyed by mfg_id+model — backend-agnostic,
│                             so a monitor scanned once via ddcutil is still a cache hit under the
│                             native backend and vice versa
├── backend/
│   ├── mod.rs            — the DdcBackend trait — the seam for going native (see below)
│   ├── ddcutil.rs          — shells out to `ddcutil`, parses its text output
│   └── native.rs            — Linux only: talks `/dev/i2c-*` directly via `ddc`/`ddc-i2c`
├── components/            — Slider / Selector / Action, the three control kinds
└── screens/                — controls / picker / raw-VCP screen rendering
```

`vcp.rs` and everything above it (`app`, `ui`, `commands`, `components`,
`screens`) know nothing about `ddcutil` or subprocesses — they only talk to
the `DdcBackend` trait. That boundary is the whole point of this rewrite.

## Why port this from Go at all

Two reasons, in order of importance:

1. **Dropping `ddcutil` as a subprocess dependency** in favor of talking
   DDC/CI natively, per OS — Linux via `/dev/i2c-*` (done, see below),
   Windows via the Monitor Configuration Functions API, macOS via IOKit
   (limited, especially on Apple Silicon — DDC access there is a known
   rough edge industry-wide, not something this project can route around).
   That's a native, per-platform, sometimes-unsafe-FFI problem, which is
   Rust's home turf in a way it isn't Go's.
2. General preference for Rust for a personal tool like this.

The `DdcBackend` trait in `backend/mod.rs` is where that work lands: each
OS becomes its own `impl DdcBackend`, selected at runtime — `main.rs` tries
the native backend first and falls back to `ddcutil` where no native one
exists yet (or it finds nothing) — with zero changes required to `app`,
`ui`, or `vcp`.

`backend::native` (Linux) doesn't hand-roll the DDC/CI or MCCS
capability-string protocol itself — that's the error-prone part `ddcutil`
gets right, and there's no reason to redo it by hand. It builds on
`ddc`/`ddc-i2c` (the i2c-dev transport) plus `mccs`/`mccs-caps`/`mccs-db`
(VESA MCCS capability parsing and the spec's feature-code database, the
same tables `ddcutil` itself is built on). Enumeration is its own
`/dev/i2c-*` scan rather than `ddc-i2c`'s built-in `with-linux-enumerate`,
to avoid pulling in `libudev`/`pkg-config` as a build dependency.

## Porting status

Ported and structurally complete, compiler-verified:

- `vcp` data model, `backend::ddcutil` (detect/capabilities/getvcp/setvcp
  parsing), `cache` — these are pure data + text parsing, translated
  fairly mechanically from the Go originals' regexes and control flow.
- `commands` (the async command layer), `app` (state + key/message
  handling), `components`, `screens`, `ui`, `main` — translated from
  `model.go`/`rawview.go`/`picker.go`/the `components` package.
- **The Go project's test suite**, in full: all 83 of its original tests
  across `internal/ddc/{getvcp,capabilities,cache}_test.go` and
  `internal/tui/{model,rawview,components/selector}_test.go` have a 1:1
  named counterpart here (`TestUpdate_RawEditing_EscCancels` →
  `raw_editing_esc_cancels`, etc.), diffed function-by-function against
  the Go originals to confirm. `cargo test` runs 126 in total now — the
  other 43 came with everything added since (mouse, the native-backend
  fixes, the visual redesign, logging).

Since the Go port:

- **A native Linux backend** (`backend::native`) landed — talks
  `/dev/i2c-*` directly, no `ddcutil` subprocess. Verified against real
  hardware (an LG ultrawide over `/dev/i2c-2`: detect, capabilities parse,
  and a `getvcp`/brightness read all check out). `main.rs` prefers it and
  falls back to the `ddcutil` backend automatically. Windows and macOS
  native backends are still open — see "Why port this from Go at all".
  `backend::native` has one `#[ignore]`d manual smoke test
  (`manual_detect_probe`) since there's no hardware in CI to exercise it
  automatically; run it with `cargo test -- --ignored --nocapture` on a
  machine with a DDC/CI monitor attached.

Left to do:

- Other vendors are supported in principle (the model was never
  LG-specific) but only exercised against one panel so far.
- Windows and macOS native backends (see "Why port this from Go at all").

## Build & run

On Linux, needs your user in the `i2c` group so `/dev/i2c-*` is readable
without `sudo` — the native backend needs that regardless of `ddcutil`.
`ddcutil` itself only needs to be installed as a fallback, for the case
where the native backend finds nothing (e.g. permissions, an unsupported
bus) or on a platform without a native backend yet.

```bash
cargo build --release
./target/release/cmmrs
```

## Keybindings

Same as the Go original:

| Key | Action |
| --- | --- |
| `↑`/`k`, `↓`/`j` | Move focus between controls |
| `←`/`h`, `→`/`l` | Adjust the focused slider or cycle the focused selector |
| `enter` | Run the focused action (opens a confirmation prompt first) |
| `y` / `n` / `esc` | Confirm / cancel a pending action |
| `v` | Toggle the Raw VCP screen |
| `r` | Refresh (re-detect + re-read current values) |
| `R` | Full rescan — drops the on-disk cache and rediscovers everything |
| `D` | Switch display (only active with more than one detected) |
| `q` / `ctrl+c` | Quit |

Inside the Raw VCP screen: `↑↓`/`j`/`k` move the focused row, `f`/`pgdn`
and `b`/`pgup` page, `e` edits the focused row's value, `r` rescans, `esc`/`v`
goes back.

## Mouse

Every screen also takes mouse input, not just the keyboard. The rule of
thumb: **scroll always navigates, click/drag on a slider's bar sets its
value.**

| Action | Effect |
| --- | --- |
| Scroll wheel, anywhere | Always moves the cursor, same as `↑`/`↓` — never touches a value, regardless of what's under the pointer |
| Click a slider's bar | Sets it to the value that column maps to (drag the same edge-to-edge scale a Go/web slider would) |
| Click a slider's name or its `NNN` value text | Focuses it only — no value change |
| Drag across a slider's bar | Follows the pointer continuously, same mapping as a click |
| Click a selector row | Focuses it and advances to its next value |
| Click an action row | Opens its confirmation prompt |
| Click a display row (Controls header, multi-display only) | Switches to that display |
| Click/scroll on the Picker or Raw VCP table | Selects a display / a feature row |

The two confirmation prompts (a destructive action, an undocumented
raw-code write) are deliberately keyboard-only — a stray click should
never be able to confirm one.

Controls/Picker also don't stretch to fill the whole terminal when there's
not enough content to need it — the box sizes to its content, growing to
fill the available height only once content doesn't fit, at which point it
scrolls (keeping the focused control in view, with a scrollbar) instead of
spilling content past the bottom border.

## Look & feel

The Controls screen groups its controls into named sections (`DISPLAY`,
`COLOR`, `AUDIO`, `POWER`, `INPUT` — a section only shows up if the monitor
actually has something in it), styled after the on-screen-display menu a
physical monitor already has — a truecolor cyan/blue palette, a slider bar
with rounded end caps, the focused row highlighted along its whole width
rather than just its label. [tachyonfx](https://github.com/junkdog/tachyonfx)
backs a few short, cell-by-cell "materialize" transitions — content decodes
in from random Braille noise (`⠋⠙⠹...`, terminal-hacker style, not a flat
color wash) the first time a scan's controls appear (~400ms) and on every
screen switch (~250ms), plus a brief color flash whenever an error
surfaces. None of it is configurable — it's a finishing touch, not a
feature surface.

## Logs

The TUI takes over the terminal, so nothing gets printed while it's
running — everything (backend selection, scan/probe results, DDC read/write
failures, cache hits/misses) goes to a log file instead:
`$XDG_CACHE_HOME/cmmrs/cmmrs.log` (`~/.cache/cmmrs/cmmrs.log` by default).
It's capped at 5 MB and starts over past that — a debugging aid, not an
audit trail.

Defaults to `info`; set `RUST_LOG=debug` (or `trace`) for more detail, e.g.
when tracking down why a control isn't showing up:

```bash
RUST_LOG=debug ./target/release/cmmrs
tail -f ~/.cache/cmmrs/cmmrs.log   # in another terminal
```

## License

[MIT](LICENSE)
