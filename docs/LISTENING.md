# Zearo listening checks (needs Matthew's ears + iD22)

Automated tests prove structure and timing. Only a human can judge whether
it grooves. Work through this list on the Audient iD22 main outs.

## Setup

```powershell
cargo run --locked -- --demo 2   # 2 bars on the preferred output, no display
cargo run --locked               # full desktop app (auto-plays the sketch)
```

## Checklist

- [ ] Demo on iD22 main outs: kick/snare/hats groove, snare answers, pluck
      line. Anything missing, distorted, or oddly balanced?
- [ ] Pads layout: hit all 10 kit pads. Each responds instantly? Velocity
      feels musical from soft taps to hard hits?
- [ ] Closed hat then open hat: does the closed hat cut off (choke)?
- [ ] Snare audition grid: all 9 takes audible and distinct? Soft/mid/hard
      bands feel right?
- [ ] Pluck keyboard: play C-E-G across two octaves. Retune clean, no clicks?
- [ ] Steps: toggle steps while playing. Timing stays solid, no glitches?
- [ ] Record a bar of kick, overdub hats without stopping. Playback matches
      what you played?
- [ ] Erase: hold Erase + a pad while playing. Only that voice is removed?
- [ ] Undo/redo/clear behave as expected, nothing else changes.
- [ ] Switch layouts mid-play (pads → steps → keys → notes → audition).
      Music continues, nothing reloads, no dropouts?
- [ ] Level/decay sliders: snare decay shortens all 9 takes at once?
- [ ] Swap a snare take for rim in the audition view. Pattern keeps playing
      the same rhythm with the new sound?
- [ ] Save, mess things up, load. Exact state restored?
- [ ] Latency feel: live pads vs pattern — tight enough for finger drumming?

## Sampling (One Shot foundations, v0.3.0)

- [ ] MIDI: play the KeyStep into the kit (C1 maps across pads) and the pluck (chromatic). Velocities feel right? Note-offs damp ringing voices?
- [ ] Record from the KeyStep with Record armed — timing matches what you played?
- [ ] Import: drop a WAV in `imports/`, Refresh, load it onto a pad. Immediate and in tune with the kit?
- [ ] Record a take from the iD22 input. Auto-trim sensible, level matched?
- [ ] Trim start/end + Apply: does the edit do what you expect without clicks?
- [ ] Take assigned to a new target survives save/load and still plays?
- [ ] Mute buttons: mute a track mid-play — silent but the loop position holds?

## Known limits (this milestone)

- WASAPI only; ASIO unproven (needs Steinberg SDK + clang).
- Main monitor pair preferred automatically (Output 1/2 on the iD22).
- No MIDI input mapping yet (device listing only); pads are mouse/touch.
- No sample import yet — all sounds are generated in code.
- 8 tracks exist; arranger depth comes later.
