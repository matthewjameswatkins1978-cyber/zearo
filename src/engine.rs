//! Realtime instrument engine.
//!
//! The audio thread owns an [`Engine`] with a preallocated voice pool. The UI
//! thread builds a [`PreparedInstrument`] off-thread and swaps it in through
//! [`Shared`]. Voices reference sample buffers by [`Arc`]; when a voice ends,
//! its buffer goes into a bounded retire queue drained by the UI thread, so
//! the audio thread never frees audio memory itself. If the queue is full
//! (pathological edit storms), the drop happens inline and is counted in
//! [`Engine::retired_dropped`] — visible, not silent.
//!
//! Per-trigger work is allocation-free by construction: no `Vec`, `HashMap`
//! or `String` operations on the trigger/render path (RR counters and buffer
//! maps are preallocated at prepare/swap time).

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use rtrb::{Consumer, Producer};

use crate::instrument::{velocity_gain, Instrument, TargetId};
use crate::sample::{AssetId, AssetPool};

/// Maximum simultaneous voices. This is polyphony headroom, not a limit on
/// instrument size: targets and samples are unbounded.
pub const MAX_VOICES: usize = 64;

/// Retire-queue depth for finished voice buffers.
pub const RETIRE_CAPACITY: usize = 512;

/// Live trigger queue depth (UI -> audio).
pub const TRIGGER_CAPACITY: usize = 1024;

// ---------------------------------------------------------------------------
// Prepared (immutable, UI-built, audio-read) instrument
// ---------------------------------------------------------------------------

pub struct PreparedZone {
    pub lo: u8,
    pub hi: u8,
    pub takes: Vec<Arc<Vec<f32>>>,
}

pub struct PreparedRange {
    pub lo_note: u8,
    pub hi_note: u8,
    pub root_note: u8,
    pub zones: Vec<PreparedZone>,
}

pub enum PreparedSelector {
    OneShot { source: Arc<Vec<f32>> },
    Velocity { zones: Vec<PreparedZone> },
    Pitched { ranges: Vec<PreparedRange> },
}

pub struct PreparedTarget {
    pub id: TargetId,
    pub name: String,
    pub level: f32,
    pub decay: f32,
    pub choke: Option<u8>,
    pub pitch_tracking: bool,
    pub root_note: u8,
    pub selector: PreparedSelector,
}

pub struct PreparedInstrument {
    pub name: String,
    pub rev: u64,
    pub targets: Vec<PreparedTarget>,
}

impl PreparedInstrument {
    pub fn target(&self, id: TargetId) -> Option<&PreparedTarget> {
        self.targets.iter().find(|t| t.id == id)
    }

    /// Every (target, zone) key the engine must preallocate an RR counter for.
    /// Mirrors the key scheme in `instrument.rs` (`(id, zone)` and
    /// `(id, range * 16 + zone)`).
    pub fn rr_keys(&self) -> Vec<(u32, u32)> {
        let mut keys = Vec::new();
        for t in &self.targets {
            match &t.selector {
                PreparedSelector::OneShot { .. } => keys.push((t.id.0, 0)),
                PreparedSelector::Velocity { zones } => {
                    for (zi, _) in zones.iter().enumerate() {
                        keys.push((t.id.0, zi as u32));
                    }
                }
                PreparedSelector::Pitched { ranges } => {
                    for (ri, r) in ranges.iter().enumerate() {
                        for (zi, _) in r.zones.iter().enumerate() {
                            keys.push((t.id.0, (ri * 16 + zi) as u32));
                        }
                    }
                }
            }
        }
        keys
    }
}

fn semitone_rate(note: u8, root: u8) -> f32 {
    2f32.powf((f32::from(note) - f32::from(root)) / 12.0)
}

/// Build the immutable prepared form off the audio thread. Missing assets
/// are reported, never silently dropped into silence... they resolve to
/// `Err` so the caller can fix the pool instead of shipping a quiet target.
pub fn prepare(
    inst: &Instrument,
    pool: &AssetPool,
    rev: u64,
) -> Result<PreparedInstrument, Vec<AssetId>> {
    let mut missing = Vec::new();
    let take = |id: AssetId, missing: &mut Vec<AssetId>| -> Option<Arc<Vec<f32>>> {
        match pool.get(id) {
            Some(a) => Some(Arc::clone(&a.pcm)),
            None => {
                if !missing.contains(&id) {
                    missing.push(id);
                }
                None
            }
        }
    };
    let mut targets = Vec::with_capacity(inst.targets.len());
    for t in &inst.targets {
        let prep_zones = |zones: &[crate::instrument::VelocityZone],
                          missing: &mut Vec<AssetId>|
         -> Vec<PreparedZone> {
            zones
                .iter()
                .map(|z| PreparedZone {
                    lo: z.lo,
                    hi: z.hi,
                    takes: z.takes.iter().filter_map(|id| take(*id, missing)).collect(),
                })
                .collect()
        };
        let selector = match &t.selector {
            crate::instrument::Selector::OneShot { source } => match take(*source, &mut missing) {
                Some(source) => PreparedSelector::OneShot { source },
                None => continue,
            },
            crate::instrument::Selector::Velocity { zones } => PreparedSelector::Velocity {
                zones: prep_zones(zones, &mut missing),
            },
            crate::instrument::Selector::Pitched { ranges } => PreparedSelector::Pitched {
                ranges: ranges
                    .iter()
                    .map(|r| PreparedRange {
                        lo_note: r.lo_note,
                        hi_note: r.hi_note,
                        root_note: r.root_note,
                        zones: prep_zones(&r.zones, &mut missing),
                    })
                    .collect(),
            },
        };
        targets.push(PreparedTarget {
            id: t.id,
            name: t.name.clone(),
            level: t.level,
            decay: t.decay,
            choke: t.choke_group,
            pitch_tracking: t.pitch_tracking,
            root_note: t.root_note,
            selector,
        });
    }
    if missing.is_empty() {
        Ok(PreparedInstrument {
            name: inst.name.clone(),
            rev,
            targets,
        })
    } else {
        Err(missing)
    }
}

// ---------------------------------------------------------------------------
// Live messages (UI -> audio, lock-free)
// ---------------------------------------------------------------------------

/// Messages from the UI thread to the audio thread. Plain data only — the
/// sequencer owns all musical state, so transport changes also travel here
/// instead of touching shared state from the UI.
#[derive(Clone, Copy, Debug)]
pub enum Msg {
    Hit {
        target: TargetId,
        note: u8,
        velocity: u8,
    },
    Erase {
        target: TargetId,
    },
    Undo,
    Redo,
    Clear,
    Play,
    Stop,
    SetBpm {
        bpm: f64,
    },
    SetArmed {
        armed: bool,
        overdub: bool,
    },
}

// ---------------------------------------------------------------------------
// Shared handles (both threads)
// ---------------------------------------------------------------------------

/// Handles shared between the UI thread (writer) and the audio thread
/// (reader). The audio thread also pushes retired buffers here.
pub struct Shared {
    pub prepared: ArcSwap<PreparedInstrument>,
}

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

struct Voice {
    active: bool,
    buf: Option<Arc<Vec<f32>>>,
    pos: f32,
    rate: f32,
    gain: f32,
    decay_k: f32,
    elapsed: f32,
    choke: Option<u8>,
    age: u64,
}

impl Voice {
    fn idle() -> Self {
        Self {
            active: false,
            buf: None,
            pos: 0.0,
            rate: 1.0,
            gain: 0.0,
            decay_k: 0.0,
            elapsed: 0.0,
            choke: None,
            age: 0,
        }
    }
}

fn finish_voice(voice: &mut Voice, retire: &mut Producer<Arc<Vec<f32>>>, dropped: &mut u64) {
    if let Some(buf) = voice.buf.take() {
        // `push` is lock-free; only the full-queue fallback drops inline.
        if retire.push(buf).is_err() {
            *dropped += 1;
        }
    }
    voice.active = false;
}

// ---------------------------------------------------------------------------
// Engine (audio thread)
// ---------------------------------------------------------------------------

pub struct Engine {
    prepared: Arc<PreparedInstrument>,
    voices: Vec<Voice>,
    rr: HashMap<(u32, u32), usize>,
    retire_tx: Producer<Arc<Vec<f32>>>,
    /// Buffers dropped inline because the retire queue was full.
    pub retired_dropped: u64,
    /// Times the oldest voice was stolen (pool exhausted).
    pub steals: u64,
    age: u64,
    sample_rate: f32,
    /// Asset rate (44.1 kHz) over device rate: keeps pitch right on 48 kHz
    /// devices without resampling the immutable assets.
    rate_factor: f32,
}

impl Engine {
    pub fn new(
        prepared: Arc<PreparedInstrument>,
        sample_rate: f32,
    ) -> (Self, Consumer<Arc<Vec<f32>>>) {
        let (retire_tx, retire_rx) = rtrb::RingBuffer::new(RETIRE_CAPACITY);
        let mut engine = Self {
            prepared,
            voices: (0..MAX_VOICES).map(|_| Voice::idle()).collect(),
            rr: HashMap::new(),
            retire_tx,
            retired_dropped: 0,
            steals: 0,
            age: 0,
            sample_rate,
            rate_factor: crate::sample::SAMPLE_RATE as f32 / sample_rate,
        };
        engine.rebuild_rr();
        (engine, retire_rx)
    }

    fn rebuild_rr(&mut self) {
        self.rr.clear();
        for key in self.prepared.rr_keys() {
            self.rr.insert(key, 0);
        }
    }

    /// Pick up a UI-side swap. Lock-free; never blocks the caller.
    pub fn sync(&mut self, shared: &Shared) {
        let current = shared.prepared.load();
        if !Arc::ptr_eq(&current, &self.prepared) {
            self.prepared = Arc::clone(&current);
            self.rebuild_rr();
        }
    }

    pub fn revision(&self) -> u64 {
        self.prepared.rev
    }

    fn kill_choke_group(&mut self, group: u8) {
        for voice in self.voices.iter_mut().filter(|v| v.active) {
            if voice.choke == Some(group) {
                finish_voice(voice, &mut self.retire_tx, &mut self.retired_dropped);
            }
        }
    }

    fn free_voice_index(&mut self) -> usize {
        if let Some(i) = self.voices.iter().position(|v| !v.active) {
            return i;
        }
        // Steal the oldest voice; its buffer retires normally.
        let mut oldest = 0;
        for (i, v) in self.voices.iter().enumerate() {
            if v.age < self.voices[oldest].age {
                oldest = i;
            }
        }
        let _ = oldest;
        self.steals += 1;
        finish_voice(
            &mut self.voices[oldest],
            &mut self.retire_tx,
            &mut self.retired_dropped,
        );
        oldest
    }

    /// Trigger a note. Returns false for unknown targets and velocity 0.
    /// Allocation-free on the hot path: RR counters are preallocated, takes
    /// are `Arc` clones, and selection only borrows the prepared form.
    /// The velocity/key walk mirrors `Instrument::resolve`; the
    /// `conformance_with_resolve` test pins them together.
    pub fn trigger(&mut self, target: TargetId, note: u8, velocity: u8) -> bool {
        if velocity == 0 {
            return false;
        }
        let index = match self.prepared.targets.iter().position(|t| t.id == target) {
            Some(i) => i,
            None => return false,
        };
        let choke = self.prepared.targets[index].choke;
        if let Some(group) = choke {
            self.kill_choke_group(group);
        }
        let (take, rate, level, decay) = {
            let t = &self.prepared.targets[index];
            let level = t.level;
            let decay = t.decay;
            let (take, rate) = match &t.selector {
                PreparedSelector::OneShot { source } => {
                    let rate = if t.pitch_tracking {
                        semitone_rate(note, t.root_note)
                    } else {
                        1.0
                    };
                    (Arc::clone(source), rate)
                }
                PreparedSelector::Velocity { zones } => {
                    let found = zones
                        .iter()
                        .enumerate()
                        .find(|(_, z)| velocity >= z.lo && velocity <= z.hi);
                    let (zi, zone) = match found {
                        Some(found) => found,
                        None => return false,
                    };
                    let counter = match self.rr.get_mut(&(t.id.0, zi as u32)) {
                        Some(counter) => counter,
                        None => return false,
                    };
                    if zone.takes.is_empty() {
                        return false;
                    }
                    let take = Arc::clone(&zone.takes[*counter % zone.takes.len()]);
                    *counter = counter.wrapping_add(1);
                    (take, 1.0)
                }
                PreparedSelector::Pitched { ranges } => {
                    let found = ranges
                        .iter()
                        .enumerate()
                        .find(|(_, r)| note >= r.lo_note && note <= r.hi_note);
                    let (ri, range) = match found {
                        Some(found) => found,
                        None => return false,
                    };
                    let zfound = range
                        .zones
                        .iter()
                        .enumerate()
                        .find(|(_, z)| velocity >= z.lo && velocity <= z.hi);
                    let (zi, zone) = match zfound {
                        Some(zfound) => zfound,
                        None => return false,
                    };
                    let counter = match self.rr.get_mut(&(t.id.0, (ri * 16 + zi) as u32)) {
                        Some(counter) => counter,
                        None => return false,
                    };
                    if zone.takes.is_empty() {
                        return false;
                    }
                    let take = Arc::clone(&zone.takes[*counter % zone.takes.len()]);
                    *counter = counter.wrapping_add(1);
                    (take, semitone_rate(note, range.root_note))
                }
            };
            (take, rate, level, decay)
        };
        self.start_voice(take, rate, gain_of(level, velocity), decay, choke);
        true
    }

    /// Retune the engine to the real device rate. Call after opening the
    /// stream, before transport starts.
    pub fn set_device_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.rate_factor = crate::sample::SAMPLE_RATE as f32 / sample_rate;
    }

    /// Start one voice on a concrete buffer.
    fn start_voice(
        &mut self,
        buf: Arc<Vec<f32>>,
        rate: f32,
        gain: f32,
        decay: f32,
        choke: Option<u8>,
    ) {
        let i = self.free_voice_index();
        self.age = self.age.wrapping_add(1);
        let decay_k = if decay >= 1.0 {
            0.0
        } else {
            3.0 * (1.0 / decay - 1.0)
        };
        let voice = &mut self.voices[i];
        voice.active = true;
        voice.buf = Some(buf);
        voice.pos = 0.0;
        voice.rate = rate * self.rate_factor;
        voice.gain = gain;
        voice.decay_k = decay_k;
        voice.elapsed = 0.0;
        voice.choke = choke;
        voice.age = self.age;
    }

    /// Render one frame. Returns (left, right); sources are mono.
    pub fn render_frame(&mut self) -> (f32, f32) {
        let mut mix = 0.0f32;
        for voice in self.voices.iter_mut() {
            if !voice.active {
                continue;
            }
            let done = {
                let buf = voice.buf.as_ref().expect("active voice holds a buffer");
                let i = voice.pos as usize;
                if i + 1 >= buf.len() {
                    true
                } else {
                    let frac = voice.pos - i as f32;
                    let s = buf[i] * (1.0 - frac) + buf[i + 1] * frac;
                    voice.pos += voice.rate;
                    voice.elapsed += 1.0 / self.sample_rate;
                    let env = if voice.decay_k > 0.0 {
                        (-voice.elapsed * voice.decay_k).exp()
                    } else {
                        1.0
                    };
                    mix += s * voice.gain * env;
                    false
                }
            };
            if done {
                finish_voice(voice, &mut self.retire_tx, &mut self.retired_dropped);
            }
        }
        // Gentle rational soft-clip keeps loud stacks musical.
        let s = mix * 0.8;
        let soft = s / (1.0 + s.abs());
        (soft, soft)
    }

    /// Render a whole block (tests, offline bounce).
    pub fn render_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        assert_eq!(left.len(), right.len());
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (a, b) = self.render_frame();
            *l = a;
            *r = b;
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }
}

fn gain_of(level: f32, velocity: u8) -> f32 {
    velocity_gain(velocity) * level
}

/// Drain retired buffers on the UI thread. Returns buffers freed.
pub fn drain_retired(rx: &mut Consumer<Arc<Vec<f32>>>) -> usize {
    let mut n = 0;
    while rx.pop().is_ok() {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{percussion_kit, pitched_pluck, select_take, snare9};
    use crate::sample::rms;

    fn render_hit(
        inst: &Instrument,
        pool: &AssetPool,
        target: TargetId,
        note: u8,
        velocity: u8,
        frames: usize,
    ) -> Vec<f32> {
        let prepared = Arc::new(prepare(inst, pool, 1).expect("prepares"));
        let (mut engine, _rx) = Engine::new(prepared, 44100.0);
        assert!(engine.trigger(target, note, velocity), "trigger lands");
        let mut left = vec![0.0; frames];
        let mut right = vec![0.0; frames];
        engine.render_block(&mut left, &mut right);
        assert_eq!(left, right, "mono sources render dual-mono");
        left
    }

    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|s| s * s).sum()
    }

    #[test]
    fn all_three_archetypes_sound_through_one_engine() {
        let pool = AssetPool::generate();
        let kit = percussion_kit(&pool);
        let kick = render_hit(&kit, &pool, TargetId(0), 60, 100, 22_050);
        assert!(energy(&kick[0..1000]) > 0.01, "kick attacks immediately");
        assert!(rms(&kick) > 0.05, "kick has body");

        let snare = snare9(&pool);
        let hit = render_hit(&snare, &pool, TargetId(0), 60, 64, 15_435);
        assert!(energy(&hit[0..500]) > 0.005, "snare attacks");

        let pluck = pitched_pluck(&pool);
        let c4 = render_hit(&pluck, &pool, TargetId(0), 60, 100, 4000);
        let d4 = render_hit(&pluck, &pool, TargetId(0), 62, 100, 4000);
        assert!(energy(&c4) > 0.001 && energy(&d4) > 0.001, "pitched sounds");
        assert!(
            crate::sample::zero_crossing_rate(&d4) > crate::sample::zero_crossing_rate(&c4) * 1.05,
            "a whole step up really retunes"
        );
    }

    #[test]
    fn conformance_with_resolve() {
        // The engine mirrors `Instrument::resolve`; pin them together.
        let pool = AssetPool::generate();
        // (target, note, velocities to sweep)
        type Probe = (TargetId, u8, Vec<u8>);
        let cases: Vec<(Instrument, Vec<Probe>)> = vec![
            (
                percussion_kit(&pool),
                (0..10).map(|t| (TargetId(t), 60, vec![10, 100])).collect(),
            ),
            (
                snare9(&pool),
                vec![(TargetId(0), 60, vec![1, 50, 51, 100, 101, 127])],
            ),
            (
                pitched_pluck(&pool),
                vec![
                    (TargetId(0), 48, vec![40, 100]),
                    (TargetId(0), 62, vec![100]),
                ],
            ),
        ];
        for (inst, probes) in &cases {
            let prepared = Arc::new(prepare(inst, &pool, 1).expect("prepares"));
            let (mut engine, _rx) = Engine::new(prepared, 44100.0);
            let mut counters: HashMap<(u32, u32), usize> = HashMap::new();
            for (target, note, vels) in probes {
                for _ in 0..5 {
                    for velocity in vels {
                        let resolved = inst.resolve(*target, *note, *velocity).expect("resolves");
                        assert!(engine.trigger(*target, *note, *velocity));
                        let voice = engine
                            .voices
                            .iter()
                            .filter(|v| v.active)
                            .max_by_key(|v| v.age)
                            .expect("new voice");
                        let key = resolved.rr_key;
                        let counter = counters.entry(key).or_insert(0);
                        let expected = select_take(&resolved.takes, counter).expect("take");
                        let expected_buf = &pool.get(expected).unwrap().pcm;
                        assert!(
                            Arc::ptr_eq(voice.buf.as_ref().expect("buffer"), expected_buf),
                            "engine picks the resolved take"
                        );
                        let expected_gain =
                            velocity_gain(*velocity) * inst.target(*target).unwrap().level;
                        assert!(
                            (voice.gain - expected_gain).abs() < 1e-6,
                            "engine applies target level and velocity"
                        );
                        assert!(
                            (voice.rate - resolved.rate).abs() < 1e-6,
                            "engine applies resolved pitch rate"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn choke_cuts_open_hat() {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let prepared = Arc::new(prepare(&inst, &pool, 1).expect("prepares"));
        let (mut engine, _rx) = Engine::new(prepared, 44100.0);
        assert!(engine.trigger(TargetId(3), 60, 100), "open hat");
        let mut scratch = vec![0.0; 2000];
        let mut scratch2 = vec![0.0; 2000];
        engine.render_block(&mut scratch, &mut scratch2);
        assert_eq!(engine.active_voices(), 1, "open hat still ringing");
        assert!(energy(&scratch) > 0.001);
        assert!(engine.trigger(TargetId(2), 60, 100), "closed hat chokes");
        assert_eq!(engine.active_voices(), 1, "open hat was cut");
    }

    #[test]
    fn swap_preserves_ringing_voices() {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let prepared = Arc::new(prepare(&inst, &pool, 1).expect("prepares"));
        let shared = Shared {
            prepared: ArcSwap::from(prepared),
        };
        let initial = shared.prepared.load();
        let (mut engine, mut rx) = Engine::new(Arc::clone(&initial), 44100.0);
        assert!(engine.trigger(TargetId(9), 60, 100), "crash (long tail)");
        let mut a = vec![0.0; 500];
        let mut b = vec![0.0; 500];
        engine.render_block(&mut a, &mut b);
        assert_eq!(engine.active_voices(), 1);

        // UI rebuilds off-thread and swaps; the ringing crash must survive.
        let mut louder = inst.clone();
        louder.target_mut(TargetId(0)).unwrap().level = 0.2;
        let prepared2 = Arc::new(prepare(&louder, &pool, 2).expect("prepares"));
        shared.prepared.store(prepared2);
        engine.sync(&shared);
        assert_eq!(engine.revision(), 2);
        assert_eq!(engine.active_voices(), 1, "old voice survives the swap");
        assert!(engine.trigger(TargetId(0), 60, 100), "new kick");
        let newest = engine
            .voices
            .iter()
            .filter(|v| v.active)
            .max_by_key(|v| v.age)
            .expect("kick voice");
        assert!(
            (newest.gain - velocity_gain(100) * 0.2).abs() < 1e-6,
            "new triggers use the new revision"
        );
        // Run everything out; buffers retire through the queue, none inline.
        let mut tail_l = vec![0.0; 70_000];
        let mut tail_r = vec![0.0; 70_000];
        engine.render_block(&mut tail_l, &mut tail_r);
        assert_eq!(engine.active_voices(), 0);
        assert!(drain_retired(&mut rx) >= 2, "finished buffers retire");
        assert_eq!(engine.retired_dropped, 0, "no inline drops");
    }

    #[test]
    fn voice_steal_is_counted() {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let prepared = Arc::new(prepare(&inst, &pool, 1).expect("prepares"));
        let (mut engine, _rx) = Engine::new(prepared, 44100.0);
        for _ in 0..70 {
            assert!(engine.trigger(TargetId(5), 60, 100));
        }
        assert_eq!(engine.active_voices(), MAX_VOICES);
        assert_eq!(engine.steals, 6, "oldest voices stolen and counted");
    }

    #[test]
    fn decay_control_shortens_tails() {
        let pool = AssetPool::generate();
        let mut inst = snare9(&pool);
        inst.target_mut(TargetId(0)).unwrap().decay = 0.25;
        let short = render_hit(&inst, &pool, TargetId(0), 60, 90, 15_435);
        let inst = snare9(&pool);
        let full = render_hit(&inst, &pool, TargetId(0), 60, 90, 15_435);
        assert!(energy(&short) < energy(&full) * 0.8, "decay shortens");
    }

    #[test]
    fn rejects_unknown_targets_and_note_off() {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let prepared = Arc::new(prepare(&inst, &pool, 1).expect("prepares"));
        let (mut engine, _rx) = Engine::new(prepared, 44100.0);
        assert!(!engine.trigger(TargetId(999), 60, 100));
        assert!(!engine.trigger(TargetId(0), 60, 0));
        assert_eq!(engine.active_voices(), 0);
    }

    #[test]
    fn prepare_reports_missing_assets() {
        let pool = AssetPool::generate();
        let mut inst = percussion_kit(&pool);
        inst.target_mut(TargetId(0)).unwrap().selector = crate::instrument::Selector::OneShot {
            source: AssetId(424242),
        };
        assert!(prepare(&inst, &pool, 1).is_err());
    }

    #[test]
    fn device_rate_retunes_without_resampling_assets() {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let prepared = Arc::new(prepare(&inst, &pool, 1).expect("prepares"));
        let (mut engine, _rx) = Engine::new(prepared, 44100.0);
        engine.set_device_rate(48000.0);
        assert!(engine.trigger(TargetId(5), 60, 100));
        let voice = engine.voices.iter().find(|v| v.active).expect("voice");
        assert!(
            (voice.rate - 44100.0 / 48000.0).abs() < 1e-6,
            "rate {}",
            voice.rate
        );
    }
}
