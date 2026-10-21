//! Musical time: patterns, transport, recording and undo.
//!
//! All mutation happens on the audio thread inside [`Sequencer::process_block`];
//! the UI only sends [`Msg`]s. Ticks are integer PPQ positions (no float drift
//! in stored data); the transport carries the fractional position between
//! audio frames. Timing derives from the audio clock, never the frame rate.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use arc_swap::ArcSwap;
use rtrb::Consumer;
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Msg};
use crate::instrument::TargetId;

/// Ticks per quarter note. 16th steps land on exact integers (240 ticks).
pub const PPQ: u64 = 960;

/// One musical event: stable target identity + pitch/expression + time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub target: TargetId,
    pub note: u8,
    pub velocity: u8,
    pub tick: u64,
}

/// A looping pattern. Events reference [`TargetId`], so pad layouts and
/// sounds can change without touching the music.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub name: String,
    pub length_ticks: u64,
    pub events: Vec<Event>,
    next_id: u64,
    /// Bumped on every mutation; the UI polls it for display snapshots.
    pub rev: u64,
}

impl Pattern {
    pub fn new(name: &str, bars: u32) -> Self {
        Self {
            name: name.to_string(),
            length_ticks: u64::from(bars) * 4 * PPQ,
            events: Vec::new(),
            next_id: 1,
            rev: 0,
        }
    }

    pub fn add(&mut self, target: TargetId, note: u8, velocity: u8, tick: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.events.push(Event {
            id,
            target,
            note,
            velocity,
            tick,
        });
        self.rev += 1;
        id
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.events.len();
        self.events.retain(|e| e.id != id);
        if self.events.len() != before {
            self.rev += 1;
            true
        } else {
            false
        }
    }

    /// Remove `target` events within ±`half_window` ticks of `center`
    /// (loop-aware). Returns the removed count.
    pub fn erase_window(&mut self, target: TargetId, center: u64, half_window: u64) -> usize {
        let before = self.events.len();
        let len = self.length_ticks.max(1);
        self.events.retain(|e| {
            if e.target != target {
                return true;
            }
            let dist = (e.tick as i64 - center as i64)
                .rem_euclid(len as i64)
                .unsigned_abs();
            let wrapped = len - dist;
            let nearest = dist.min(wrapped);
            nearest > half_window
        });
        let removed = before - self.events.len();
        if removed > 0 {
            self.rev += 1;
        }
        removed
    }

    pub fn clear(&mut self) {
        if !self.events.is_empty() {
            self.events.clear();
            self.rev += 1;
        }
    }

    /// Toggle a step: remove the nearest matching event at `tick`, else add.
    pub fn toggle(&mut self, target: TargetId, note: u8, velocity: u8, tick: u64) -> bool {
        if let Some(id) = self
            .events
            .iter()
            .find(|e| e.target == target && e.tick == tick)
            .map(|e| e.id)
        {
            self.remove(id);
            false
        } else {
            self.add(target, note, velocity, tick);
            true
        }
    }
}

/// Playback position in fractional ticks. Advanced by audio frames.
#[derive(Debug, Clone)]
pub struct Transport {
    pub bpm: f64,
    pub sample_rate: f64,
    pub tick: f64,
    pub playing: bool,
}

impl Transport {
    pub fn new(bpm: f64, sample_rate: f64) -> Self {
        Self {
            bpm,
            sample_rate,
            tick: 0.0,
            playing: false,
        }
    }

    pub fn ticks_per_frame(&self) -> f64 {
        PPQ as f64 * self.bpm / 60.0 / self.sample_rate
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn stop(&mut self) {
        self.playing = false;
    }
}

/// UI-side read handle for the live pattern. The audio thread publishes a
/// clone only when [`Pattern::rev`] changes (user edits), never per block;
/// the UI renders from this snapshot and never touches sequencer state.
pub struct LiveViews {
    pub pattern: ArcSwap<Pattern>,
    pub published_rev: AtomicU64,
}

impl LiveViews {
    pub fn new(pattern: &Pattern) -> Self {
        Self {
            pattern: ArcSwap::from(Arc::new(pattern.clone())),
            published_rev: AtomicU64::new(pattern.rev),
        }
    }
}

/// Bounded undo over whole-pattern snapshots. Patterns are small; a clone
/// per user gesture is cheaper than an event-sourced journal and trivially
/// correct. Only the audio thread touches these.
#[derive(Debug, Default)]
pub struct UndoStack {
    undo: Vec<Pattern>,
    redo: Vec<Pattern>,
}

impl UndoStack {
    const CAP: usize = 64;

    pub fn checkpoint(&mut self, pattern: &Pattern) {
        self.undo.push(pattern.clone());
        if self.undo.len() > Self::CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, current: Pattern) -> Option<Pattern> {
        let previous = self.undo.pop()?;
        self.redo.push(current);
        Some(previous)
    }

    pub fn redo(&mut self, current: Pattern) -> Option<Pattern> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        Some(next)
    }

    pub fn depth(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }
}

/// How the record button behaves (MPC distinction that matters):
/// Record captures and auto-starts from the top when stopped; Overdub only
/// adds while the loop is already playing. Erase is a held modifier + pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArmMode {
    #[default]
    Off,
    Record,
    Overdub,
}

/// The playing/recording musical state. Owned by the audio callback.
pub struct Sequencer {
    pub transport: Transport,
    pub pattern: Pattern,
    pub undo: UndoStack,
    pub arm: ArmMode,
    scratch: Vec<(usize, usize)>,
}

impl Sequencer {
    pub fn new(bpm: f64, sample_rate: f64, pattern: Pattern) -> Self {
        Self {
            transport: Transport::new(bpm, sample_rate),
            pattern,
            undo: UndoStack::default(),
            arm: ArmMode::Off,
            scratch: Vec::new(),
        }
    }

    pub fn set_bpm(&mut self, bpm: f64) {
        self.transport.bpm = bpm.clamp(30.0, 300.0);
    }

    /// Publish a display snapshot when the pattern changed. Called from the
    /// audio thread after each block; cheap when nothing changed.
    pub fn publish(&self, views: &LiveViews) {
        let rev = self.pattern.rev;
        if views.published_rev.load(Ordering::Relaxed) != rev {
            views.pattern.store(Arc::new(self.pattern.clone()));
            views.published_rev.store(rev, Ordering::Relaxed);
        }
    }

    /// One audio block: schedule pattern events by the audio clock, apply UI
    /// messages (live hits record at the exact frame), render. Live hits
    /// sound even when the transport is stopped; only pattern playback and
    /// recording need `playing`.
    pub fn process_block(
        &mut self,
        engine: &mut Engine,
        msgs: &mut Consumer<Msg>,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        assert_eq!(left.len(), right.len());
        let n = left.len();
        let tpf = self.transport.ticks_per_frame();
        let len = self.pattern.length_ticks.max(1) as f64;

        // Collect due events for this block (loop-aware, frame offsets).
        self.scratch.clear();
        if self.transport.playing {
            let t0 = self.transport.tick;
            for (i, e) in self.pattern.events.iter().enumerate() {
                let rel = (e.tick as f64 - t0).rem_euclid(len);
                if rel < tpf * n as f64 {
                    let frame = ((rel / tpf) as usize).min(n.saturating_sub(1));
                    self.scratch.push((frame, i));
                }
            }
            self.scratch.sort_unstable();
        }

        let mut si = 0;
        for f in 0..n {
            while si < self.scratch.len() && self.scratch[si].0 == f {
                let e = &self.pattern.events[self.scratch[si].1];
                engine.trigger(e.target, e.note, e.velocity);
                si += 1;
            }
            while let Ok(msg) = msgs.pop() {
                let now = self.transport.tick as u64;
                match msg {
                    Msg::Hit {
                        target,
                        note,
                        velocity,
                    } => {
                        engine.trigger(target, note, velocity);
                        if self.arm != ArmMode::Off && self.transport.playing {
                            self.undo.checkpoint(&self.pattern);
                            self.pattern.add(target, note, velocity, now);
                        }
                    }
                    Msg::Erase { target } => {
                        if self.transport.playing {
                            self.undo.checkpoint(&self.pattern);
                            self.pattern.erase_window(target, now, PPQ / 8);
                        }
                    }
                    Msg::Undo => {
                        if self.undo.depth().0 > 0 {
                            let current = std::mem::replace(&mut self.pattern, Pattern::new("", 1));
                            if let Some(previous) = self.undo.undo(current) {
                                self.pattern = previous;
                            }
                        }
                    }
                    Msg::Redo => {
                        if self.undo.depth().1 > 0 {
                            let current = std::mem::replace(&mut self.pattern, Pattern::new("", 1));
                            if let Some(next) = self.undo.redo(current) {
                                self.pattern = next;
                            }
                        }
                    }
                    Msg::Clear => {
                        self.undo.checkpoint(&self.pattern);
                        self.pattern.clear();
                    }
                    Msg::Play => self.transport.play(),
                    Msg::Stop => self.transport.stop(),
                    Msg::SetBpm { bpm } => self.set_bpm(bpm),
                    Msg::SetArmed { armed, overdub } => {
                        self.arm = match (armed, overdub) {
                            (true, true) => ArmMode::Overdub,
                            (true, false) => ArmMode::Record,
                            (false, _) => ArmMode::Off,
                        };
                    }
                }
            }
            let (l, r) = engine.render_frame();
            left[f] = l;
            right[f] = r;
            if self.transport.playing {
                self.transport.tick += tpf;
                if self.transport.tick >= len {
                    self.transport.tick -= len;
                    // Snap the loop phase: the residue is float error far
                    // below one audio frame. Without this, events at tick 0
                    // would schedule at the END of the next block
                    // (rel ≈ len − ε) instead of frame 0.
                    if self.transport.tick < tpf {
                        self.transport.tick = 0.0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::prepare;
    use crate::instrument::percussion_kit;
    use crate::sample::AssetPool;
    use std::sync::Arc;

    const SR: f64 = 44100.0;
    const BPM: f64 = 120.0;

    /// One bar at 120bpm/44.1k = exactly 88200 frames.
    fn bar_frames() -> usize {
        (SR * 60.0 / BPM * 4.0) as usize
    }

    fn harness() -> (Sequencer, Engine, rtrb::Producer<Msg>, AssetPool) {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let prepared = Arc::new(prepare(&inst, &pool, 1).expect("prepares"));
        let (engine, _rx) = Engine::new(prepared, SR as f32);
        let (tx, _) = rtrb::RingBuffer::new(1024);
        let pattern = Pattern::new("test", 1);
        let mut seq = Sequencer::new(BPM, SR, pattern);
        seq.transport.play();
        (seq, engine, tx, pool)
    }

    #[test]
    fn scheduler_is_frame_exact() {
        let (mut seq, mut engine, _tx, _pool) = harness();
        // Kick on step 1 (second 16th, tick 240).
        seq.pattern.add(TargetId(0), 60, 100, 240);
        let n = bar_frames();
        let mut left = vec![0.0; n];
        let mut right = vec![0.0; n];
        let (mut dummy_tx, mut dummy_rx) = rtrb::RingBuffer::new(8);
        let _ = &mut dummy_tx;
        seq.process_block(&mut engine, &mut dummy_rx, &mut left, &mut right);
        // 16th at 120bpm = 0.125s = 5512.5 frames -> floor 5512.
        assert!(
            left[..5512].iter().all(|s| *s == 0.0),
            "silence before the hit"
        );
        // Frame 5512 holds fade-in sample zero by design; the attack lands
        // within the next few frames.
        assert!(
            left[5512..5520].iter().any(|s| *s != 0.0),
            "kick attacks at frame 5512"
        );
    }

    #[test]
    fn loop_repeats_and_overdub_preserves_ticks() {
        let (mut seq, mut engine, _tx, _pool) = harness();
        seq.pattern.add(TargetId(0), 60, 100, 0);
        seq.pattern.add(TargetId(1), 60, 90, 480);
        // Overdub two hats without touching the originals.
        seq.arm = ArmMode::Overdub;
        let (mut msg_tx, mut msg_rx) = rtrb::RingBuffer::new(8);
        msg_tx
            .push(Msg::Hit {
                target: TargetId(2),
                note: 60,
                velocity: 80,
            })
            .unwrap();
        let n = bar_frames();
        let mut left = vec![0.0; n];
        let mut right = vec![0.0; n];
        seq.process_block(&mut engine, &mut msg_rx, &mut left, &mut right);
        // Loop-phase regression is pinned by the bit-exact check below.
        let originals: Vec<u64> = seq
            .pattern
            .events
            .iter()
            .filter(|e| e.target == TargetId(0) || e.target == TargetId(1))
            .map(|e| e.tick)
            .collect();
        assert_eq!(originals, vec![0, 480], "originals bit-identical");
        assert_eq!(
            seq.pattern
                .events
                .iter()
                .filter(|e| e.target == TargetId(2))
                .count(),
            1,
            "overdub landed exactly once"
        );
        // Second bar sounds the same loop again.
        let mut left2 = vec![0.0; n];
        let mut right2 = vec![0.0; n];
        seq.process_block(&mut engine, &mut msg_rx, &mut left2, &mut right2);
        // Regression: float wrap residue must not postpone tick-0 events —
        // the loop restarts sample-exact.
        let energy = |b: &[f32]| b.iter().map(|s| s * s).sum::<f32>();
        eprintln!(
            "DEBUG energy bar1: {}",
            left.iter().map(|s| s * s).sum::<f32>()
        );
        let ratio = energy(&left2) / energy(&left).max(1e-9);
        assert!((ratio - 1.0).abs() < 0.05, "loop repeats, ratio {ratio}");
    }

    #[test]
    fn erase_and_undo_restore() {
        let (mut seq, mut engine, _tx, _pool) = harness();
        seq.pattern.add(TargetId(0), 60, 100, 0);
        seq.pattern.add(TargetId(0), 60, 100, 240);
        seq.undo.checkpoint(&seq.pattern);
        let removed = seq.pattern.erase_window(TargetId(0), 10, PPQ / 8);
        assert_eq!(removed, 1, "only the event near the playhead goes");
        assert_eq!(seq.pattern.events.len(), 1);
        let current = std::mem::replace(&mut seq.pattern, Pattern::new("", 1));
        seq.pattern = seq.undo.undo(current).expect("undo works");
        assert_eq!(seq.pattern.events.len(), 2, "undo restores");
        let _ = (&mut seq, &mut engine);
    }

    #[test]
    fn record_quantize_free_timing() {
        // Recording keeps the exact frame-derived tick (no hidden quantize).
        let (mut seq, mut engine, _tx, _pool) = harness();
        seq.arm = ArmMode::Record;
        let (mut msg_tx, mut msg_rx) = rtrb::RingBuffer::new(8);
        // Hit pushed before the block lands at frame 0 -> tick 0.
        msg_tx
            .push(Msg::Hit {
                target: TargetId(4),
                note: 60,
                velocity: 70,
            })
            .unwrap();
        let mut left = vec![0.0; 100];
        let mut right = vec![0.0; 100];
        seq.process_block(&mut engine, &mut msg_rx, &mut left, &mut right);
        assert_eq!(seq.pattern.events.len(), 1);
        assert_eq!(seq.pattern.events[0].tick, 0, "frame-0 hit records tick 0");
        // Advance half a 16th, then hit: tick must be ~275, not snapped to 240.
        let mut left = vec![0.0; 2756];
        let mut right = vec![0.0; 2756];
        seq.process_block(&mut engine, &mut msg_rx, &mut left, &mut right);
        msg_tx
            .push(Msg::Hit {
                target: TargetId(4),
                note: 60,
                velocity: 70,
            })
            .unwrap();
        let mut left = vec![0.0; 10];
        let mut right = vec![0.0; 10];
        seq.process_block(&mut engine, &mut msg_rx, &mut left, &mut right);
        let tick = seq.pattern.events[1].tick;
        assert!(
            tick > 100 && tick != 240 && tick != 480,
            "unquantized tick {tick}"
        );
    }

    #[test]
    fn tempo_scales_scheduling() {
        let (mut seq, mut engine, _tx, _pool) = harness();
        seq.set_bpm(60.0);
        seq.pattern.add(TargetId(0), 60, 100, 240);
        let n = bar_frames() * 2;
        let mut left = vec![0.0; n];
        let mut right = vec![0.0; n];
        let (_, mut msg_rx) = rtrb::RingBuffer::<Msg>::new(8);
        seq.process_block(&mut engine, &mut msg_rx, &mut left, &mut right);
        let onset = left.iter().position(|s| *s != 0.0).expect("hit sounds");
        assert!(
            (11025..=11029).contains(&onset),
            "half tempo = double the frames, got {onset}"
        );
    }
}
