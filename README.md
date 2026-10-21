# Zearo

Product name **Zearo**; package and repository name **`zearo`** (lowercase).
A Rust-first groovebox: real-time audio, MIDI, DSP and a live workbench UI.

> Public visibility is **not** an open-source grant. No licence has been
> selected yet — do not add one without reviewing our licensing intentions.

## Stack (verified 2026-10-21)

| Layer | Crate | Version | Why |
| --- | --- | --- | --- |
| Audio I/O | `cpal` | 0.18.2 | Standard low-level cross-platform audio I/O |
| MIDI | `midir` | 0.11.1 | Maintained cross-platform realtime MIDI |
| DSP / synth | `fundsp` | 0.23.0 | Maintained audio synthesis and processing graph |
| UI | `egui` + `eframe` | 0.37.0 | Immediate-mode GUI, native + web, suits live audio tools |
| Realtime messaging | `rtrb` | 0.3.5 | Lock-free SPSC ring buffer for the audio thread |
| Benchmarking | `criterion` | 0.8.2 | Statistics-driven micro-benchmarks |
| Testing | `cargo-nextest` | 0.9.140 | Fast parallel test runner (already installed) |

Rust 1.99.1 stable, no C++ or JUCE. See `docs/SETUP.md` for the full
workbench record and `Cargo.lock` for the exact pinned tree.

## Quick verify

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --locked
cargo test --locked
cargo run --quiet          # headless probe: DSP + devices, no hardware needed
cargo run --quiet -- --ui  # opens the minimal workbench window
```

`cargo run` never requires audio/MIDI hardware: device lists may be empty
and playback is not attempted. Opening a stream or sounding audio is a
later milestone, verified on real hardware then.
