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

## Known limits (this milestone)

- WASAPI only; ASIO unproven (needs Steinberg SDK + clang).
- Main monitor pair preferred automatically (Output 1/2 on the iD22).
- No MIDI input mapping yet (device listing only); pads are mouse/touch.
- No sample import yet — all sounds are generated in code.
- 8 tracks exist; arranger depth comes later.
