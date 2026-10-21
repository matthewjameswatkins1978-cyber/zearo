# Zearo architecture assessment — Packet 002

First playable prototype: verified 2026-10-21 on the physical Audient iD22
(Output 1/2 @ 48 kHz, 3 bars, peak 0.413, 0 stream errors) and heard by
Matthew through both the headless demo and the desktop app.

## The model as built

```
Instrument (TargetId → Target → Selector)   stable musical identity
  + AssetPool (recipes → immutable PCM)      immutable sound media
  ──prepare (off-thread)──▶ PreparedInstrument (Arcs + RR slots)
  ──ArcSwap──▶ Engine (64 preallocated voices, choke, decay)
Pattern (events → TargetId, PPQ ticks)      music, separate from sound
Transport + Sequencer (audio-clocked)       timing, recording, undo
Layout (pure view mapping)                  pads / steps / keys / notes
Song (8 tracks, versioned JSON doc)         save / load
```

One engine renders a one-shot kit voice, a 9-take snare and a pitched
multisample through the same trigger path. Patterns reference stable
target IDs; layouts map gestures to targets; the audio thread owns voices
while the UI owns everything else. State crosses the boundary only via
lock-free messages, atomic swaps, and immutable snapshots.

## What worked (with evidence)

- **Same core, three behaviours.** `percussion_kit`, `snare9` and
  `pitched_pluck` all resolve through `Instrument::resolve` and render
  through one `Engine::trigger`. A conformance test pins every
  zone/pitch/velocity to identical buffer + gain + rate on both paths.
- **Velocity + round-robin.** Boundary tests (50→soft, 51→mid),
  deterministic 7-trigger RR cycles, shared single `decay` for all nine
  takes. Adding the pitched instrument needed only a new `Selector`
  variant — no engine, UI, or pattern changes.
- **Non-destructive everything.** `replace_source` swaps one take; the
  pattern is untouched (test asserts identical event streams and an
  audible difference). Prepared swaps keep ringing voices alive (old
  buffers retire through the queue; the test counts zero inline drops).
- **Timing from the audio clock.** Frame-exact onset test, bit-deterministic
  loop restart, unquantized recording test. A float wrap-residue bug that
  postponed tick-0 events a full bar was *caught by the repeat test* and
  fixed with a sub-frame loop-phase snap.
- **Real-time safety by construction.** No locks, no filesystem, no per-note
  allocation on the trigger/render path; RR counters and buffers are
  preallocated. `retired_dropped` and stream-error counters make violations
  visible instead of theoretical (both zero in every run so far).
- **Layouts are genuinely views.** Switching replaces one value; a test
  asserts the pattern is byte-identical across switches, and the app keeps
  playing through them (confirmed by ear).

## What proved awkward

- **Selection lives in two places.** `Instrument::resolve` (borrowed,
  UI-side) and `Engine::trigger` (prepared, audio-side) walk the same
  velocity/key logic. The conformance test pins them together, but this is
  the clearest duplication in the codebase — the next refactor should
  unify them into one selector interpreter over borrowed data.
- **The RR key scheme is a magic encoding** (`(id, zone)` and
  `(id, range * 16 + zone)` must match `rr_keys()` exactly). Works, tested,
  but ugly; it should become a single preallocated counter table indexed
  by the prepared walk itself.
- **Gain staging is spread across three places** (velocity curve, target
  level, master soft-clip). Sounds fine; needs one documented headroom
  policy before more instruments pile on.
- **The rate-factor retune is correct but crude.** Assets are 44.1 kHz and
  the engine scales playback rate for the device. Fine for one rate pair;
  a real sample-rate story (resampling audit, Pitch × rate interaction)
  belongs to the next milestone.
- **`Msg` grows with every UI capability.** Fine for a prototype, but at
  ~20 variants it needs a review pass — the rule stays: the UI never
  touches audio state except through messages, swaps, and snapshots.
- **Save format IDs are positional** (asset index = pool generation order).
  Robust for one machine, not yet for sharing: stable symbolic IDs or
  content hashes come before any project exchange.

## What should change (in order)

1. Unify selection: one walk over borrowed takes, used by both resolve
   paths; replace the `(id, zone)` key hack with walk-indexed counters.
2. Document the headroom policy (velocity curve × level × master) once.
3. Stable asset identities for the save format (names or hashes, not pool
   order) + a format-version migration path (currently: reject loudly).
4. ASIO evaluation on the real iD22 (needs Steinberg SDK + clang) only if
   WASAPI latency proves musically limiting — measure first.
5. MIDI input mapping (devices enumerate; nothing plays them yet).

## Validation matrix (research proposal) — status

Exercised: one-shots, 9-snare (3×3), sliced-break equivalent
(non-destructive spans + regions; slicing UI still to come), layered
acoustic+electronic kick (clap-over-snare in the demo groove), loop
record + overdub while swapping sounds, linked-clip editing (clip IS the
pattern — no copies exist). Open: 200-sample piano scale, articulations/
releases, synth patch without WAV, plugin/CLAP adapters, multi-mic phase
locking, missing-asset recovery drills, portable project exchange.

## The most important question

**Does the universal model make ordinary musical operations genuinely
simpler, or merely hide the complexity?**

Judged from the build, not the diagram: the model paid off where music
happens. Adding the pitched instrument took one enum variant and zero
engine/UI/pattern changes. Auditioning nine takes fell out of
`audition_sources` in an afternoon. Swapping a snare for a rim mid-loop
was a one-line model edit with the rhythm provably untouched. Recording,
erasing, undo and layout switches all operate on stable IDs and none of
them know what a "kit" is — yet the musician still sees a familiar kit.

The hidden complexity is real but bounded and named: the mirrored
selection walk, the RR key encoding, positional save IDs, the crude rate
retune. None of it leaked into the musician's workflow; all of it is
covered by tests that fail loudly if the two sides drift. The model is
not elegant yet — but the complexity sits in the engine room with gauges
on it, not behind the pads. Simplify the selector core next, keep the
surfaces as they are.
