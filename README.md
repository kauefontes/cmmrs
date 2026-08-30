# vcpctl

A terminal UI for controlling DDC/CI monitors — any vendor, any monitor
that speaks MCCS. Rust port of a prior Go project
([`lg-control-tui`](../../go/lg-control-tui)) that was accidentally named
after the one monitor it was built against, even though nothing about it
was ever LG-specific.

The point of this tool: everything a monitor exposes over DDC/CI —
brightness, contrast, RGB gain, input source, color presets, power mode,
speaker volume, whatever a given panel happens to have — controllable from
a keyboard-driven TUI, without diving into an OSD menu with a joystick
button to find it.

**Status: skeleton / early port, not yet built or run.** This was
translated file-by-file from the Go original by hand; expect the first
`cargo build` to need a few fixes. See "Porting status" below.

## Architecture

```
src/
├── main.rs         — terminal setup/teardown, the event loop
├── app.rs           — application state + update logic (the Elm-style Model)
├── ui.rs             — top-level render dispatch, shared box chrome
├── commands.rs        — async work: spawns a thread per backend call, reports back via mpsc
├── vcp.rs              — backend-agnostic data model (Display, Capabilities, VcpFeature, FeatureReading)
├── cache.rs             — on-disk, versioned scan cache
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

Ported and structurally complete (pending a compiler to confirm):

- `vcp` data model, `backend::ddcutil` (detect/capabilities/getvcp/setvcp
  parsing), `cache` — these are pure data + text parsing, translated
  fairly mechanically from the Go originals' regexes and control flow.
- `commands` (the async command layer), `app` (state + key/message
  handling), `components`, `screens`, `ui`, `main` — translated from
  `model.go`/`rawview.go`/`picker.go`/the `components` package, but this is
  the part most likely to need real fixes once it hits a compiler: some
  ratatui API surface (`Frame`'s lifetime, `Line`/`Text` construction
  details) was written from memory/docs, not verified against the crate.

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

Not yet ported:

- **The Go project's test suite.** Its `ddc` package tests parse fixture
  text captured from real `ddcutil` output on real hardware; its `tui`
  tests drive `Model.Update` with synthetic messages. Both port
  cleanly — the *behavior* is already fully specified by the Go code and
  tests — this just hasn't been done yet. `backend::ddcutil` has a couple
  of smoke tests as a starting point.
- **Raw VCP screen scrolling** is a hand-rolled offset (`app.raw_scroll`)
  rather than a proper scrollable widget — functional but unpolished
  compared to the Go original's `bubbles/viewport`.
- Other vendors are supported in principle (the model was never
  LG-specific) but only exercised against one panel so far.

## Build & run

On Linux, needs your user in the `i2c` group so `/dev/i2c-*` is readable
without `sudo` — the native backend needs that regardless of `ddcutil`.
`ddcutil` itself only needs to be installed as a fallback, for the case
where the native backend finds nothing (e.g. permissions, an unsupported
bus) or on a platform without a native backend yet.

```bash
cargo build --release
./target/release/vcpctl
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
