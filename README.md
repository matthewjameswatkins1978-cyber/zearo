# Zearo

**An eight-track groovebox for catching an idea before it gets away.**

Zearo is a desktop-first musical instrument built in Rust: an 8×8 pad workbench, real-time audio engine, patterns, recording, and several ways to play the same sounds. It takes inspiration from hands-on hardware grooveboxes, with an emphasis on **play → capture → develop**, not endless menu archaeology.

It's a **playable prototype (0.3.0)**, not yet a polished music workstation.

## What you can try today

- **Eight tracks and an 8×8 grid.** Start with a demo sketch containing a kit groove, a multi-take snare and a pitched pluck line. The remaining tracks are available for experiments.
- **One musical pattern, different views.** Switch between Pads, Steps, Keys, Notes and Audition layouts without duplicating or altering your notes.
- **Play, edit and record.** The audio-clocked sequencer supports pattern playback, live recording/overdub and undo. The workbench also offers audio input capture.
- **Change the sound without destroying the performance.** Patterns refer to stable instrument targets. The engine handles one-shots, velocity zones, round-robin takes and pitched samples through a shared rendering path.
- **See the audio path work.** Headless probe, output demo and input tests make it possible to separate audio-device problems from the graphical app.

The current sample assets are generated for the demo. This isn't a finished sound library or a complete hardware integration.

## Run it

**Primary development platform:** Windows 11 with a stable Rust toolchain and MSVC build tools. The project has also been designed with portability in mind, but other platform builds and audio paths are not yet verified to the same standard.

Clone and run from the repository:

```powershell
git clone https://github.com/matthewjameswatkins1978-cyber/zearo.git
cd zearo
cargo run --locked
```

The application opens the desktop workbench and attempts to use a suitable audio output. You'll want speakers or headphones connected.

You can also check the audio path without opening the UI:

```powershell
cargo run --locked -- --probe          # Audio/MIDI device names; opens no stream
cargo run --locked -- --demo 2         # Play two bars of the demo on your output
cargo run --locked -- --input-test 2   # Capture two seconds of input, report levels
cargo run --locked -- --help
```

`--probe` is safe to run without audio hardware. The demo and input test require working devices. A successful compile is not proof that your particular sound card or driver will behave correctly.

## The central idea

Zearo treats **instruments, patterns and layouts as different things**.

```text
Instrument + prepared sound assets  →  real-time voice engine
Pattern (target IDs + timed events) →  audio-clocked sequencer
Layout (pads / steps / keys / notes)→  different ways to interact
Song                               →  eight tracks and project state
```

Change a drum sound without rewriting the rhythm. Switch from pads to keys without making a second clip. Keep allocations and file I/O away from the time-critical audio callback.

That architecture is already exercised by deterministic tests and a real-hardware listening pass. The trade-offs and remaining rough edges are documented in [the architecture assessment](docs/ARCHITECTURE.md).

## What isn't ready

This is where the ambition meets the actual repository:

- MIDI devices can be enumerated, but **live MIDI input mapping is not yet a complete playing workflow**.
- A full sampler slicing interface, plug-in hosting, a flexible synthesizer and song-arrangement workflow are **not yet delivered**.
- The saved song format has limitations around portable asset identities and migration.
- Linux ARM64 / Raspberry Pi builds need system-level audio dependencies and proper target verification.
- Audio-driver latency, broader device support and long-session robustness still need musical testing.

None of those are secretly “coming soon” features you can currently click.

## Develop and verify

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --locked
cargo test --locked
```

The [setup record](docs/SETUP.md) contains tested toolchain details; [listening checks](docs/LISTENING.md) cover the human side of audio validation.

**Technology:** Rust, CPAL, midir, fundsp, egui/eframe, and lock-free `rtrb` queues. No JUCE or C++ application core.

**Licence:** Not yet selected. The repository is public for visibility, but no permission to reuse the source should be inferred until a licence is added.
