# Zearo workbench setup

Verified 2026-10-21 on Windows 11 x86_64. Another agent should be able to
recreate this environment from this file + `rust-toolchain.toml` + `Cargo.lock`.

## What is pinned where

- `rust-toolchain.toml` — `channel = "stable"` plus required components and
  targets. Deliberately not an exact-version pin, so this project never forces
  a duplicate toolchain download next to the workshop's shared stable.
- `Cargo.lock` — committed. The exact 539-crate tree. Build/test with
  `--locked` so CI and humans resolve identically.
- This file — the actual verified versions (below). If a version drifts,
  update this file in the same commit.

## Verified versions (2026-10-21)

| Tool | Version | Note |
| --- | --- | --- |
| `rustc` / `cargo` | 1.99.1 stable (`58cf8f9c3`) | MSVC host |
| `rustfmt` | 1.10.0-stable | via toolchain component |
| `clippy` | 0.1.99 | via toolchain component |
| targets | `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (std only) | |
| `git` | 2.55.0.windows.5 | |
| `gh` | 2.102.0 | auth: `matthewjameswatkins1978-cyber` |
| `pi` | 1.3.0 | installed, current npm latest |
| `opencode` | v2.0.32 | installed, current |
| `threadmoth` | 1.20.0 | all repo edits go through it |
| `node` / `npm` | v24.19.0 / 11.17.0 | runtime for pi/opencode |
| `cargo-nextest` | 0.9.140 | test runner |
| `cargo-audit` | 0.22.2 | advisories |
| VS Build Tools 2022 | 17.14 + Windows SDK 10.0.22621.0 | native MSVC prerequisite |
| `cpal` / `midir` / `fundsp` / `egui+eframe` / `rtrb` / `criterion` | 0.18.2 / 0.11.1 / 0.23.0 / 0.37.0 / 0.3.5 / 0.8.2 | see `Cargo.lock` |

Rust 1.99.0 → 1.99.1 was pulled by a `rustup` channel sync on first build
(patch release, no action needed). No other global upgrades were made.

## Install from scratch

1. Install VS Build Tools 2022 with the C++ workload + a Windows 10/11 SDK.
2. Install `rustup` (stable), then in this directory run `cargo fetch` —
   `rust-toolchain.toml` auto-installs missing components/targets.
3. `gh auth login`, `cargo install cargo-nextest --locked` (optional).
4. No C++ libraries, no JUCE, no nightly toolchain. Ever.

## Verify (all must pass)

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --locked
cargo test --locked
cargo bench --locked -- --test   # compiles benches, one quick pass each
cargo run --locked --quiet -- --probe  # devices only, no hardware needed
cargo run --locked --quiet -- --demo 2  # 2 bars on the preferred output
cargo run --locked                      # desktop app (grid + recording)
```

`--probe` never opens a stream, so empty device lists on headless machines
are fine. `--demo` and the app open the preferred output (iD22 main pair
when present; WASAPI) — verified 2026-10-21 on the real iD22 at 48 kHz
with zero stream errors, and heard by Matthew. See `docs/LISTENING.md`
for the human checklist and `docs/ARCHITECTURE.md` for the assessment.

## Linux ARM64 (Raspberry Pi) status

- `aarch64-unknown-linux-gnu` std library is installed.
- Full `cargo check --target aarch64-unknown-linux-gnu` currently fails at
  `alsa-sys`: no Linux sysroot / cross `pkg-config` on this host.
- Next step when needed (not done now, to avoid scope creep): `cargo-xwin`
  or a WSL2 sysroot, then re-run the target check. Pure-DSP code
  (`fundsp`, `rtrb`, `zearo` lib logic) has no platform dependency by design.

## Repo

- https://github.com/matthewjameswatkins1978-cyber/zearo (public)
- `main` tracks `origin/main`. Product name **Zearo**, package/repo `zearo`.
- No licence selected: public visibility is not an open-source grant.
