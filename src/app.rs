//! Desktop workbench: 8×8 grid, interchangeable layouts, MPC-style transport.
//!
//! The UI never touches audio state directly: it sends [`Msg`]s into the
//! audio thread, swaps prepared instruments through [`Shared`], and reads
//! display snapshots from [`LiveViews`]. Layouts are pure view values —
//! switching one never touches the instrument or the pattern.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use arc_swap::ArcSwap;
use eframe::egui;
use rtrb::{Consumer, Producer};

use crate::audio::{self, LiveTrack, RunningAudio};
use crate::engine::{drain_retired, prepare, Engine, Msg, Shared};
use crate::instrument::{percussion_kit, pitched_pluck, snare9, Instrument, Selector, TargetId};
use crate::layout::{Layout, PadAction};
use crate::midi::{self, MidiBridge};
use crate::pattern::{LiveViews, Pattern, Sequencer};
use crate::sample::{AssetId, AssetPool};
use crate::song::{self, Song};

const SAVE_FILE: &str = "zearo-song.json";

/// One live track: audio-thread parts move into the callback, the UI keeps
/// the handles below.
pub struct TrackHandles {
    pub shared: Arc<Shared>,
    pub views: Arc<LiveViews>,
    pub msg_tx: Producer<Msg>,
    pub retire_rx: Consumer<Arc<Vec<f32>>>,
    pub retired: u64,
}

/// Demo project: kit groove + snare answers + pluck line on tracks 1-3.
pub fn demo_project() -> (Song, AssetPool) {
    let pool = AssetPool::generate();
    let mut song = Song::new("Zearo sketch", 120.0);

    let mut groove = Pattern::new("groove", 1);
    let step = |s: u64| s * 240;
    for s in [0, 8] {
        groove.add(TargetId(0), 60, 100, step(s));
    }
    for s in [4, 12] {
        groove.add(TargetId(1), 60, 95, step(s));
    }
    groove.add(TargetId(4), 60, 70, step(12));
    for s in [0, 2, 4, 6, 8, 10, 12] {
        groove.add(TargetId(2), 60, if s % 4 == 0 { 90 } else { 75 }, step(s));
    }
    groove.add(TargetId(3), 60, 90, step(14));
    for s in [2, 6, 10, 14] {
        groove.add(TargetId(8), 60, 60, step(s));
    }
    groove.add(TargetId(11), 60, 80, step(6));
    groove.add(TargetId(10), 60, 90, step(10));
    song.tracks[0].name = "Kit".to_string();
    song.tracks[0].instrument = Some(percussion_kit(&pool));
    song.tracks[0].pattern = Some(groove);

    let mut backbeat = Pattern::new("snare", 1);
    for s in [4, 12] {
        backbeat.add(TargetId(0), 60, 110, step(s));
    }
    for (s, v) in [(2, 45), (7, 55), (10, 50)] {
        backbeat.add(TargetId(0), 60, v, step(s));
    }
    song.tracks[1].name = "Snare".to_string();
    song.tracks[1].instrument = Some(snare9(&pool));
    song.tracks[1].pattern = Some(backbeat);

    let mut line = Pattern::new("pluck", 1);
    for (s, n) in [
        (0, 48),
        (2, 51),
        (4, 55),
        (6, 57),
        (8, 60),
        (10, 57),
        (12, 55),
        (14, 36),
    ] {
        let vel = if n < 40 { 90 } else { 88 };
        line.add(TargetId(0), n, vel, step(s));
    }
    song.tracks[2].name = "Pluck".to_string();
    song.tracks[2].instrument = Some(pitched_pluck(&pool));
    song.tracks[2].pattern = Some(line);

    (song, pool)
}

fn empty_instrument() -> Instrument {
    Instrument {
        name: "—".to_string(),
        targets: vec![],
        asset_names: vec![],
    }
}

/// Build live audio state for every track (silent where empty). The stream
/// never restarts when sounds change — only prepared snapshots swap.
pub fn spawn_all(song: &Song, pool: &AssetPool) -> (Vec<LiveTrack>, Vec<TrackHandles>) {
    let mut live = Vec::new();
    let mut handles = Vec::new();
    for (i, track) in song.tracks.iter().enumerate() {
        let inst = track.instrument.clone().unwrap_or_else(empty_instrument);
        let pattern = track
            .pattern
            .clone()
            .unwrap_or_else(|| Pattern::new("empty", 1));
        let prepared = Arc::new(prepare(&inst, pool, i as u64 + 1).expect("demo prepares"));
        let shared = Arc::new(Shared {
            prepared: ArcSwap::from(Arc::clone(&prepared)),
        });
        let (engine, retire_rx) = Engine::new(prepared, 44100.0);
        let seq = Sequencer::new(song.bpm, 44100.0, pattern.clone());
        let views = Arc::new(LiveViews::new(&pattern));
        let (msg_tx, msg_rx) = rtrb::RingBuffer::new(1024);
        live.push(LiveTrack {
            engine,
            seq,
            msgs: msg_rx,
            shared: Arc::clone(&shared),
        });
        handles.push(TrackHandles {
            shared,
            views,
            msg_tx,
            retire_rx,
            retired: 0,
        });
    }
    let _ = pool;
    (live, handles)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutKind {
    Pads,
    Steps,
    Keys,
    Notes,
    Audition,
}

impl LayoutKind {
    fn label(self) -> &'static str {
        match self {
            Self::Pads => "Pads",
            Self::Steps => "Steps",
            Self::Keys => "Keys",
            Self::Notes => "Notes",
            Self::Audition => "Audition",
        }
    }
}

/// Hand-rolled hue → RGB so pad grammar never depends on a color API.
fn target_color(id: u32, glow: f32) -> egui::Color32 {
    let h = (id as f32 * 47.0 + 10.0) % 360.0;
    let s = 0.65;
    let v = (0.22 + 0.68 * glow).min(1.0);
    let c = (h / 60.0).floor();
    let f = h / 60.0 - c;
    let (r, g, b) = match c as u32 % 6 {
        0 => (v, v * (1.0 - s * (1.0 - f)), v * (1.0 - s)),
        1 => (v * (1.0 - s * f), v, v * (1.0 - s)),
        2 => (v * (1.0 - s), v, v * (1.0 - s * (1.0 - f))),
        3 => (v * (1.0 - s), v * (1.0 - s * f), v),
        4 => (v * (1.0 - s * (1.0 - f)), v * (1.0 - s), v),
        _ => (v, v * (1.0 - s), v * (1.0 - s * f)),
    };
    egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// An in-progress input-capture take. The stream delivers chunks on its
/// own thread; the UI accumulates them here, off the audio thread.
struct RecState {
    cap: crate::audio::InputCapture,
    acc: Vec<f32>,
    max_frames: usize,
}

/// A finished take: original preserved, playback prepared separately.
#[derive(Clone)]
struct LastTake {
    name: String,
    original: Vec<f32>,
    rate: u32,
    asset: crate::sample::AssetId,
}

pub struct ZearoApp {
    pool: AssetPool,
    song: Song,
    handles: Vec<TrackHandles>,
    audio: Option<RunningAudio>,
    audio_error: Option<String>,
    active: usize,
    layout: LayoutKind,
    velocity: u8,
    keys_base: u8,
    selected: Option<TargetId>,
    selected_take_id: Option<AssetId>,
    erase: bool,
    rec: bool,
    odub: bool,
    status: String,
    prep_rev: u64,
    replace_asset: String,
    started: std::time::Instant,
    flash: HashMap<TargetId, (f64, u8)>,
    midi: Option<MidiBridge>,
    midi_status: String,
    imports: Vec<String>,
    import_pick: String,
    recording: Option<RecState>,
    last_take: Option<LastTake>,
    assigned: Option<(TargetId, crate::sample::AssetId)>,
    trim_start_ms: f32,
    trim_end_ms: f32,
    take_count: u32,
    sample_status: String,
}

impl Default for ZearoApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ZearoApp {
    pub fn new() -> Self {
        let (song, pool) = demo_project();
        let (live, handles) = spawn_all(&song, &pool);
        let views: Vec<Arc<LiveViews>> = handles.iter().map(|h| Arc::clone(&h.views)).collect();
        let mut app = Self {
            pool,
            song,
            handles,
            audio: None,
            audio_error: None,
            active: 0,
            layout: LayoutKind::Pads,
            velocity: 100,
            keys_base: 48,
            selected: Some(TargetId(0)),
            selected_take_id: None,
            erase: false,
            rec: false,
            odub: false,
            status: String::new(),
            prep_rev: 100,
            replace_asset: "rim".to_string(),
            started: std::time::Instant::now(),
            flash: HashMap::new(),
            midi: None,
            midi_status: "MIDI: not connected".to_string(),
            imports: Vec::new(),
            import_pick: String::new(),
            recording: None,
            last_take: None,
            assigned: None,
            trim_start_ms: 0.0,
            trim_end_ms: 0.0,
            take_count: 0,
            sample_status: String::new(),
        };
        match audio::start_multi(live, views) {
            Ok(running) => {
                app.status = format!(
                    "{} @ {} Hz ({} ch)",
                    running.info.name, running.info.sample_rate, running.info.channels
                );
                app.audio = Some(running);
                // Instant music: the loop starts on launch.
                for h in &mut app.handles {
                    let _ = h.msg_tx.push(Msg::Play);
                }
            }
            Err(e) => {
                app.audio_error = Some(format!("audio unavailable: {e}"));
            }
        }
        app.midi_connect();
        let _ = std::fs::create_dir_all("imports");
        app.refresh_imports();
        app
    }

    fn send(&mut self, msg: Msg) {
        if let Some(h) = self.handles.get_mut(self.active) {
            let _ = h.msg_tx.push(msg);
        }
    }

    fn send_all(&mut self, msg: Msg) {
        for h in &mut self.handles {
            let _ = h.msg_tx.push(msg.clone());
        }
    }

    fn active_instrument(&self) -> Option<&Instrument> {
        self.song.tracks.get(self.active)?.instrument.as_ref()
    }

    fn rebuild(&mut self, t: usize) {
        self.prep_rev += 1;
        let rev = self.prep_rev;
        if let (Some(inst), Some(h)) =
            (self.song.tracks[t].instrument.as_ref(), self.handles.get(t))
        {
            match prepare(inst, &self.pool, rev) {
                Ok(p) => {
                    h.shared.prepared.store(Arc::new(p));
                }
                Err(missing) => {
                    self.status = format!("prepare failed: {} missing assets", missing.len());
                }
            }
        }
    }

    fn snapshot_song(&self) -> Song {
        let mut song = self.song.clone();
        for (i, track) in song.tracks.iter_mut().enumerate() {
            if let Some(h) = self.handles.get(i) {
                track.pattern = Some((**h.views.pattern.load()).clone());
            }
        }
        song
    }

    /// (Re)connect the KeyStep (or first available input). Never panics;
    /// a missing device just reports status — the app keeps working.
    fn midi_connect(&mut self) {
        self.midi = None;
        match MidiBridge::connect_keystep_or_first() {
            Ok(bridge) => {
                self.midi_status = format!("MIDI: {}", bridge.port_name());
                self.midi = Some(bridge);
            }
            Err(e) => self.midi_status = format!("MIDI: {e}"),
        }
    }

    fn refresh_imports(&mut self) {
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir("imports") {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_wav = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false);
                if is_wav {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        names.push(name.to_string());
                    }
                }
            }
        }
        names.sort();
        self.imports = names;
        if self.import_pick.is_empty() {
            if let Some(first) = self.imports.first().cloned() {
                self.import_pick = first;
            }
        }
    }

    fn unique_take_name(&self, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while self.pool.find_by_name(&name).is_some() {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }

    /// Prepare a take, import it, remember the original. Assignment to a
    /// target happens separately so one recording can feed several slots.
    fn take_audio(&mut self, name: String, rate: u32, pcm: Vec<f32>, to_new: bool) {
        let prep = crate::sample::prepare_take(rate, &pcm);
        if prep.pcm.is_empty() {
            self.sample_status = format!("{name}: silence, nothing assigned");
            return;
        }
        let name = self.unique_take_name(&name);
        let id = self
            .pool
            .import(&name, crate::sample::SAMPLE_RATE, prep.pcm);
        self.song.samples.push(crate::song::StoredSample {
            name: name.clone(),
            sample_rate: rate,
            pcm,
        });
        let ms = self
            .song
            .samples
            .last()
            .map(|s| s.pcm.len() as f32 / rate as f32 * 1000.0)
            .unwrap_or(0.0);
        self.last_take = Some(LastTake {
            name: name.clone(),
            original: Vec::new(),
            rate,
            asset: id,
        });
        self.trim_start_ms = 0.0;
        self.trim_end_ms = ms;
        self.assigned = None;
        // Move the original into the take without another big copy.
        if let Some(take) = self.last_take.as_mut() {
            take.original = self
                .song
                .samples
                .last()
                .map(|s| s.pcm.clone())
                .unwrap_or_default();
        }
        if to_new {
            self.assign_take(true);
        } else {
            self.sample_status = format!("{name} ready — assign to a target");
        }
    }

    fn assign_take(&mut self, to_new: bool) {
        let take = match self.last_take.clone() {
            Some(t) => t,
            None => {
                self.sample_status = "nothing to assign — record or load first".into();
                return;
            }
        };
        let active = self.active;
        if to_new {
            let next = self
                .song
                .tracks
                .get(active)
                .and_then(|t| t.instrument.as_ref())
                .map(|inst| inst.targets.iter().map(|t| t.id.0).max().unwrap_or(0) + 1);
            let next = match next {
                Some(n) => n,
                None => {
                    self.sample_status = "no instrument on this track".into();
                    return;
                }
            };
            let target = TargetId(next);
            if let Some(track) = self.song.tracks.get_mut(active) {
                let inst = track
                    .instrument
                    .get_or_insert(crate::instrument::Instrument {
                        name: "Samples".to_string(),
                        targets: vec![],
                        asset_names: vec![],
                    });
                inst.targets.push(crate::instrument::Target {
                    id: target,
                    name: take.name.clone(),
                    selector: Selector::OneShot { source: take.asset },
                    level: 0.9,
                    decay: 1.0,
                    choke_group: None,
                    pitch_tracking: false,
                    root_note: 60,
                });
            }
            self.selected = Some(target);
            self.assigned = Some((target, take.asset));
            self.rebuild(active);
            self.sample_status = format!("{} on a new pad", take.name);
        } else {
            let sel = self.selected;
            let mut done = None;
            if let Some(inst) = self
                .song
                .tracks
                .get_mut(active)
                .and_then(|t| t.instrument.as_mut())
            {
                if let Some(sel) = sel {
                    if let Some(old) = inst.audition_sources(sel).first().copied() {
                        if inst.replace_source(sel, old, take.asset) {
                            done = Some((sel, take.asset));
                        }
                    }
                }
            }
            if let Some((t, id)) = done {
                self.assigned = Some((t, id));
                self.rebuild(active);
                self.sample_status = format!("{} on {}", take.name, t.0);
            } else {
                self.sample_status = "select a target with a take first".into();
            }
        }
    }

    fn apply_trim(&mut self) {
        let (take, _target, asset) = match (&self.last_take, &self.assigned) {
            (Some(t), Some((tgt, id))) => (t.clone(), *tgt, *id),
            _ => {
                self.sample_status = "assign a take first".into();
                return;
            }
        };
        let sr = take.rate as f32;
        let a = (self.trim_start_ms.max(0.0) / 1000.0 * sr) as usize;
        let b = (self.trim_end_ms.max(0.0) / 1000.0 * sr) as usize;
        let (a, b) = (a.min(take.original.len()), b.min(take.original.len()));
        if b <= a + 100 {
            self.sample_status = "trim range too small".into();
            return;
        }
        let prep = crate::sample::prepare_take(take.rate, &take.original[a..b]);
        if prep.pcm.is_empty() {
            self.sample_status = "trimmed to silence".into();
            return;
        }
        if self.pool.replace_buffer(asset, prep.pcm) {
            self.rebuild(self.active);
            self.sample_status = format!(
                "trimmed {} to {:.0}–{:.0} ms",
                take.name, self.trim_start_ms, self.trim_end_ms
            );
        }
    }

    fn start_take(&mut self) {
        if self.recording.is_some() {
            return;
        }
        match crate::audio::start_input_capture() {
            Ok(cap) => {
                let name = cap.info.name.clone();
                let max_frames = cap.info.sample_rate as usize * 10;
                self.recording = Some(RecState {
                    cap,
                    acc: Vec::new(),
                    max_frames,
                });
                self.sample_status = format!("recording from {name} …");
            }
            Err(e) => self.sample_status = format!("input failed: {e}"),
        }
    }

    fn stop_take(&mut self, auto: bool) {
        let Some(rec) = self.recording.take() else {
            return;
        };
        let rate = rec.cap.info.sample_rate;
        drop(rec.cap);
        self.take_count += 1;
        let n = self.take_count;
        if rec.acc.len() < 100 {
            self.sample_status = if auto {
                "take too short".to_string()
            } else {
                "take discarded (too short)".to_string()
            };
            return;
        }
        self.take_audio(format!("take-{n}"), rate, rec.acc, false);
    }

    fn save(&mut self) {
        let song = self.snapshot_song();
        match song::save_song(&song) {
            Ok(text) => match std::fs::write(SAVE_FILE, text) {
                Ok(()) => self.status = format!("saved {SAVE_FILE}"),
                Err(e) => self.status = format!("save failed: {e}"),
            },
            Err(e) => self.status = format!("serialize failed: {e}"),
        }
    }

    fn load(&mut self) {
        let text = match std::fs::read_to_string(SAVE_FILE) {
            Ok(text) => text,
            Err(e) => {
                self.status = format!("load failed: {e}");
                return;
            }
        };
        let song = match song::load_song(&text) {
            Ok(song) => song,
            Err(e) => {
                self.status = format!("load failed: {e}");
                return;
            }
        };
        if song.tracks.len() != self.handles.len() {
            self.status = format!(
                "track count mismatch ({}), keeping current",
                song.tracks.len()
            );
            return;
        }
        // Fresh pool every load: generate + restore in doc order, so the
        // restored ids match the takes that reference them.
        self.pool = AssetPool::generate();
        song::restore_samples(&mut self.pool, &song);
        self.song = song;
        for i in 0..self.handles.len() {
            self.rebuild(i);
            if let Some(pattern) = self.song.tracks[i].pattern.clone() {
                self.send_to(i, Msg::LoadPattern { pattern });
            }
        }
        let bpm = self.song.bpm;
        self.send_all(Msg::SetBpm { bpm });
        self.selected = self
            .active_instrument()
            .and_then(|inst| inst.targets.first().map(|t| t.id));
        self.status = format!("loaded {SAVE_FILE}");
    }

    fn send_to(&mut self, t: usize, msg: Msg) {
        if let Some(h) = self.handles.get_mut(t) {
            let _ = h.msg_tx.push(msg);
        }
    }

    fn pad_pressed(&mut self, action: PadAction, now: f64) {
        match action {
            PadAction::Hit {
                target,
                note,
                velocity,
            } => {
                if self.erase {
                    self.send(Msg::Erase { target });
                } else {
                    self.send(Msg::Hit {
                        target,
                        note,
                        velocity,
                    });
                    self.flash.insert(target, (now, velocity));
                    self.selected = Some(target);
                }
            }
            PadAction::Audition { asset } => {
                self.send(Msg::Audition { asset });
                self.selected_take_id = Some(asset);
            }
            PadAction::ToggleStep { target, tick } => {
                self.send(Msg::Toggle {
                    target,
                    note: 60,
                    velocity: self.velocity,
                    tick,
                });
            }
            PadAction::None => {}
        }
    }

    fn selected_take_name(&self) -> String {
        self.selected_take_id
            .and_then(|id| self.pool.get(id))
            .map(|a| a.name.clone())
            .unwrap_or_else(|| "\u{2014}".to_string())
    }
}

impl eframe::App for ZearoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        for h in self.handles.iter_mut() {
            h.retired += drain_retired(&mut h.retire_rx) as u64;
        }
        if self.recording.is_some() {
            let mut finished = false;
            if let Some(rec) = self.recording.as_mut() {
                while let Ok(chunk) = rec.cap.rx.try_recv() {
                    rec.acc.extend_from_slice(&chunk);
                    if rec.acc.len() >= rec.max_frames {
                        finished = true;
                        break;
                    }
                }
            }
            if finished {
                self.stop_take(true);
            }
        }
        // MIDI input: parsed + mapped on the UI thread so the single
        // audio-queue producer stays single. Plays and records like pads.
        if self.midi.is_some() {
            let mut raw = Vec::new();
            if let Some(bridge) = self.midi.as_mut() {
                raw = bridge.poll();
            }
            let mut played: Vec<(bool, TargetId, u8, u8)> = Vec::new();
            if let Some(inst) = self.active_instrument() {
                for event in &raw {
                    match event {
                        midi::MidiEvent::NoteOn { note, velocity } => {
                            if let Some((t, n, v)) = midi::map_note(inst, *note, *velocity) {
                                played.push((true, t, n, v));
                            }
                        }
                        midi::MidiEvent::NoteOff { note } => {
                            if let Some((t, n)) = midi::map_release(inst, *note) {
                                played.push((false, t, n, 0));
                            }
                        }
                    }
                }
            }
            for (on, target, note, velocity) in played {
                if on {
                    self.send(Msg::Hit {
                        target,
                        note,
                        velocity,
                    });
                    self.flash.insert(target, (now, velocity));
                    self.selected = Some(target);
                } else {
                    self.send(Msg::Release { target, note });
                }
            }
        }
        self.flash.retain(|_, (t, _)| now - *t < 0.4);

        ui.horizontal(|ui| {
            if ui.button("\u{25b6} Play").clicked() {
                self.send_all(Msg::Play);
            }
            if ui.button("\u{25a0} Stop").clicked() {
                self.send_all(Msg::Stop);
            }
            let rec_label = if self.rec {
                "\u{25cf} Rec*"
            } else {
                "\u{25cb} Rec"
            };
            if ui.button(rec_label).clicked() {
                if self.rec {
                    self.send_all(Msg::SetArmed {
                        armed: false,
                        overdub: false,
                    });
                    self.rec = false;
                } else {
                    self.send_all(Msg::Play);
                    self.send_all(Msg::SetArmed {
                        armed: true,
                        overdub: false,
                    });
                    self.rec = true;
                    self.odub = false;
                }
            }
            let od_label = if self.odub { "Overdub*" } else { "Overdub" };
            if ui.button(od_label).clicked() {
                if self.odub {
                    self.send_all(Msg::SetArmed {
                        armed: false,
                        overdub: false,
                    });
                    self.odub = false;
                } else {
                    self.send_all(Msg::Play);
                    self.send_all(Msg::SetArmed {
                        armed: true,
                        overdub: true,
                    });
                    self.odub = true;
                    self.rec = false;
                }
            }
            let erase_btn = egui::Button::new("Erase").fill(if self.erase {
                egui::Color32::DARK_RED
            } else {
                egui::Color32::TRANSPARENT
            });
            if ui.add(erase_btn).clicked() {
                self.erase = !self.erase;
            }
            if ui.button("Undo").clicked() {
                self.send(Msg::Undo);
            }
            if ui.button("Redo").clicked() {
                self.send(Msg::Redo);
            }
            if ui.button("Clear").clicked() {
                self.send(Msg::Clear);
            }
            let mut bpm = self.song.bpm;
            if ui
                .add(egui::Slider::new(&mut bpm, 30.0..=300.0).text("BPM"))
                .changed()
            {
                self.song.bpm = bpm;
                self.send_all(Msg::SetBpm { bpm });
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            if ui.button("Load").clicked() {
                self.load();
            }
        });

        ui.horizontal(|ui| {
            for (i, track) in self.song.tracks.iter().enumerate() {
                let label = format!("{} {}", i + 1, track.name);
                let mut btn = egui::Button::new(label);
                if i == self.active {
                    btn = btn.fill(egui::Color32::DARK_BLUE);
                }
                if ui.add(btn).clicked() {
                    self.active = i;
                    self.selected = self.song.tracks[i]
                        .instrument
                        .as_ref()
                        .and_then(|inst| inst.targets.first().map(|t| t.id));
                }
            }
        });

        ui.horizontal(|ui| {
            for kind in [
                LayoutKind::Pads,
                LayoutKind::Steps,
                LayoutKind::Keys,
                LayoutKind::Notes,
                LayoutKind::Audition,
            ] {
                let mut btn = egui::Button::new(kind.label());
                if self.layout == kind {
                    btn = btn.fill(egui::Color32::DARK_GREEN);
                }
                if ui.add(btn).clicked() {
                    self.layout = kind;
                }
            }
        });

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_min_width(560.0);
                self.ui_layout(ui, now);
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_min_width(420.0);
                self.ui_target_panel(ui);
                self.ui_sampling(ui);
            });
        });

        let errors: u64 = self.audio.as_ref().map(|a| a.error_count()).unwrap_or(0);
        let dropped: u64 = self
            .handles
            .iter()
            .map(|h| h.views.dropped.load(Ordering::Relaxed))
            .sum();
        let retired: u64 = self.handles.iter().map(|h| h.retired).sum();
        ui.separator();
        ui.horizontal(|ui| {
            match (&self.audio, &self.audio_error) {
                (Some(a), _) => {
                    ui.label(format!(
                        "{} @ {} Hz · stream errors {} · retired {} bufs · inline drops {}",
                        a.info.name, a.info.sample_rate, errors, retired, dropped
                    ));
                }
                (None, Some(e)) => {
                    ui.label(format!("no audio output ({e}) — UI still edits"));
                }
                (None, None) => {
                    ui.label("audio starting");
                }
            }
            ui.label(format!("· {}", self.status));
            ui.label(format!("· up {:.0}s", self.started.elapsed().as_secs_f32()));
        });
        ui.horizontal(|ui| {
            ui.label(&self.midi_status);
            if ui.button("MIDI rescan").clicked() {
                self.midi_connect();
            }
            if ui.button("Panic").clicked() {
                self.send_all(Msg::ReleaseAll);
                self.send_all(Msg::SetArmed {
                    armed: false,
                    overdub: false,
                });
                self.rec = false;
                self.odub = false;
            }
        });
        ui.ctx().request_repaint();
    }
}

impl ZearoApp {
    fn layout_value(&self) -> Layout {
        let inst = match self.active_instrument() {
            Some(inst) => inst,
            None => return Layout::Notes,
        };
        let order: Vec<TargetId> = inst.targets.iter().map(|t| t.id).collect();
        match self.layout {
            LayoutKind::Pads => Layout::pads_for(inst, self.velocity),
            LayoutKind::Steps => {
                let ticks = self
                    .handles
                    .get(self.active)
                    .map(|h| h.views.pattern.load().length_ticks)
                    .unwrap_or(3840);
                Layout::steps_for(order, ticks)
            }
            LayoutKind::Keys => Layout::Keys {
                target: self.selected.unwrap_or(TargetId(0)),
                base_note: self.keys_base,
                velocity: self.velocity,
            },
            LayoutKind::Notes => Layout::Notes,
            LayoutKind::Audition => {
                Layout::audition_for(inst, self.selected.unwrap_or(TargetId(0)))
            }
        }
    }

    fn ui_layout(&mut self, ui: &mut egui::Ui, now: f64) {
        let layout = self.layout_value();
        match self.layout {
            LayoutKind::Pads => self.ui_pads(ui, &layout, now),
            LayoutKind::Steps => self.ui_steps(ui, &layout),
            LayoutKind::Keys => self.ui_keys(ui),
            LayoutKind::Notes => self.ui_notes(ui),
            LayoutKind::Audition => self.ui_audition(ui, &layout, now),
        }
    }

    fn pad_button(&mut self, ui: &mut egui::Ui, pad: u8, layout: &Layout, now: f64) {
        let action = layout.pad_action(pad);
        let (label, color) = match &action {
            PadAction::Hit { target, .. } => {
                let glow = self
                    .flash
                    .get(target)
                    .map(|(t, _)| (1.0 - (now - *t) / 0.4) as f32)
                    .unwrap_or(0.15);
                (
                    format!("{}", target.0),
                    target_color(target.0, glow.max(0.15)),
                )
            }
            PadAction::Audition { .. } => ("\u{266a}".to_string(), egui::Color32::DARK_GRAY),
            PadAction::ToggleStep { .. } => ("\u{25a1}".to_string(), egui::Color32::DARK_GRAY),
            PadAction::None => (String::new(), egui::Color32::from_black_alpha(40)),
        };
        let enabled = !matches!(action, PadAction::None);
        let resp = ui.add_sized([54.0, 54.0], egui::Button::new(label).fill(color));
        if enabled && resp.clicked() {
            self.pad_pressed(action, now);
        }
    }

    fn ui_pads(&mut self, ui: &mut egui::Ui, layout: &Layout, now: f64) {
        egui::Grid::new("pads").spacing([4.0, 4.0]).show(ui, |ui| {
            for row in 0..8 {
                for col in 0..8 {
                    self.pad_button(ui, row * 8 + col, layout, now);
                }
                ui.end_row();
            }
        });
    }

    fn ui_steps(&mut self, ui: &mut egui::Ui, layout: &Layout) {
        let snap: Arc<Pattern> = Arc::clone(&*self.handles[self.active].views.pattern.load());
        let tick = self.handles[self.active].views.tick.load(Ordering::Relaxed);
        let ticks_per_step = snap.length_ticks.max(16) / 16;
        let cur_step = ((tick / ticks_per_step) % 16) as usize;
        let rows: Vec<TargetId> = match layout {
            Layout::Steps { rows, .. } => rows.clone(),
            _ => vec![],
        };
        let names: HashMap<TargetId, String> = self
            .active_instrument()
            .map(|inst| {
                inst.targets
                    .iter()
                    .map(|t| (t.id, t.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (vi, target) in rows.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(names.get(target).cloned().unwrap_or_default());
                for s in 0..16u64 {
                    let pad = vi as u8 * 16 + s as u8;
                    let on = snap
                        .events
                        .iter()
                        .any(|e| e.target == *target && e.tick == s * ticks_per_step);
                    let mut btn = egui::Button::new(if on { "\u{25a0}" } else { "·" });
                    if s as usize == cur_step {
                        btn = btn.fill(egui::Color32::WHITE);
                    } else if on {
                        btn = btn.fill(target_color(target.0, 0.8));
                    }
                    if ui.add_sized([26.0, 26.0], btn).clicked() {
                        if let PadAction::ToggleStep { target, tick } = layout.pad_action(pad) {
                            self.send(Msg::Toggle {
                                target,
                                note: 60,
                                velocity: self.velocity,
                                tick,
                            });
                        }
                    }
                }
            });
        }
        let _ = cur_step;
    }

    fn ui_keys(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("oct -").clicked() {
                self.keys_base = self.keys_base.saturating_sub(12);
            }
            ui.label(format!("C{}", self.keys_base as i16 / 12 - 1));
            if ui.button("oct +").clicked() {
                self.keys_base = (self.keys_base + 12).min(108);
            }
            ui.add(egui::Slider::new(&mut self.velocity, 1..=127).text("velocity"));
        });
        ui.horizontal(|ui| {
            for i in 0..16u8 {
                let note = self.keys_base.saturating_add(i);
                let name = note_name(note);
                let target = self.selected.unwrap_or(TargetId(0));
                if ui
                    .add_sized([52.0, 120.0], egui::Button::new(name))
                    .clicked()
                {
                    self.pad_pressed(
                        PadAction::Hit {
                            target,
                            note,
                            velocity: self.velocity,
                        },
                        ui.input(|in_data| in_data.time),
                    );
                }
            }
        });
    }

    fn ui_notes(&mut self, ui: &mut egui::Ui) {
        let snap: Arc<Pattern> = Arc::clone(&*self.handles[self.active].views.pattern.load());
        let names: HashMap<TargetId, String> = self
            .active_instrument()
            .map(|inst| {
                inst.targets
                    .iter()
                    .map(|t| (t.id, t.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        ui.label(format!("{} events", snap.events.len()));
        egui::ScrollArea::vertical()
            .max_height(420.0)
            .show(ui, |ui| {
                let mut remove: Option<u64> = None;
                for e in snap.events.iter().take(300) {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "#{} {} tick {} note {} vel {}",
                            e.id,
                            names.get(&e.target).cloned().unwrap_or_default(),
                            e.tick,
                            e.note,
                            e.velocity
                        ));
                        if ui.button("del").clicked() {
                            remove = Some(e.id);
                        }
                    });
                }
                if let Some(id) = remove {
                    self.send(Msg::Remove { id });
                }
            });
    }

    fn ui_audition(&mut self, ui: &mut egui::Ui, layout: &Layout, now: f64) {
        ui.label("tap a take to audition it (never recorded)");
        egui::Grid::new("takes").spacing([4.0, 4.0]).show(ui, |ui| {
            for pad in 0..16u8 {
                if pad % 4 == 0 && pad > 0 {
                    ui.end_row();
                }
                match layout.pad_action(pad) {
                    PadAction::Audition { asset } => {
                        let selected = self.selected_take_id == Some(asset);
                        let mut btn = egui::Button::new(format!("{}", pad));
                        if selected {
                            btn = btn.fill(egui::Color32::LIGHT_BLUE);
                        }
                        if ui.add_sized([52.0, 52.0], btn).clicked() {
                            self.pad_pressed(PadAction::Audition { asset }, now);
                        }
                    }
                    _ => {
                        ui.add_sized(
                            [52.0, 52.0],
                            egui::Button::new("").fill(egui::Color32::TRANSPARENT),
                        );
                    }
                }
            }
            ui.end_row();
        });
        ui.horizontal(|ui| {
            ui.label(format!("take: {}", self.selected_take_name()));
            let mut names: Vec<String> = (0..self.pool.len())
                .filter_map(|i| self.pool.get(AssetId(i as u32)))
                .map(|a| a.name.clone())
                .collect();
            names.sort();
            egui::ComboBox::from_label("swap with")
                .selected_text(&self.replace_asset)
                .show_ui(ui, |ui| {
                    for name in names {
                        ui.selectable_value(&mut self.replace_asset, name.clone(), name);
                    }
                });
            if ui.button("Swap take").clicked() {
                let swap = self
                    .selected
                    .zip(self.selected_take_id)
                    .and_then(|(t, take)| {
                        self.pool
                            .find_by_name(&self.replace_asset)
                            .map(|new| (t, take, new))
                    });
                if let Some((target, take, new)) = swap {
                    let ok = self.song.tracks[self.active]
                        .instrument
                        .as_mut()
                        .map(|inst| inst.replace_source(target, take, new))
                        .unwrap_or(false);
                    if ok {
                        self.rebuild(self.active);
                        self.status = format!("swapped take for {}", self.replace_asset);
                    } else {
                        self.status = "swap did not land".to_string();
                    }
                }
            }
        });
    }

    fn ui_target_panel(&mut self, ui: &mut egui::Ui) {
        let track_name = self
            .song
            .tracks
            .get(self.active)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        ui.heading(format!("Track {} — {track_name}", self.active + 1));
        let target_ids: Vec<(TargetId, String)> = self
            .active_instrument()
            .map(|inst| {
                inst.targets
                    .iter()
                    .map(|t| (t.id, t.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        if target_ids.is_empty() {
            ui.label("empty slot — load a sound:");
            ui.horizontal(|ui| {
                if ui.button("Kit").clicked() {
                    self.load_factory(self.active, Factory::Kit);
                }
                if ui.button("Snare").clicked() {
                    self.load_factory(self.active, Factory::Snare);
                }
                if ui.button("Pluck").clicked() {
                    self.load_factory(self.active, Factory::Pluck);
                }
            });
            return;
        }
        let mut sel = self.selected;
        let sel_name = target_ids
            .iter()
            .find(|(id, _)| Some(*id) == sel)
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        egui::ComboBox::from_label("Target")
            .selected_text(sel_name)
            .show_ui(ui, |ui| {
                for (id, name) in &target_ids {
                    ui.selectable_value(&mut sel, Some(*id), name);
                }
            });
        self.selected = sel;
        let mut changed = false;
        if let Some(inst) = self
            .song
            .tracks
            .get_mut(self.active)
            .and_then(|t| t.instrument.as_mut())
        {
            if let Some(sel_id) = sel {
                let take_count = inst.audition_sources(sel_id).len();
                if let Some(t) = inst.target_mut(sel_id) {
                    if ui
                        .add(egui::Slider::new(&mut t.level, 0.0..=1.5).text("Level"))
                        .changed()
                    {
                        changed = true;
                    }
                    if ui
                        .add(egui::Slider::new(&mut t.decay, 0.1..=1.0).text("Decay"))
                        .changed()
                    {
                        changed = true;
                    }
                    ui.label(format!(
                        "takes: {} · choke: {} · pitch-track: {}",
                        take_count,
                        t.choke_group
                            .map(|g| g.to_string())
                            .unwrap_or_else(|| "—".to_string()),
                        t.pitch_tracking
                    ));
                }
            }
        }
        if changed {
            self.rebuild(self.active);
        }
        let snap: Arc<Pattern> = Arc::clone(&*self.handles[self.active].views.pattern.load());
        ui.label(format!(
            "pattern: {} events / {} ticks",
            snap.events.len(),
            snap.length_ticks
        ));
    }

    fn ui_sampling(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("Sampling");
        ui.label("Drop .wav files into the imports/ folder.");
        ui.horizontal(|ui| {
            if ui.button("Refresh imports").clicked() {
                self.refresh_imports();
            }
            egui::ComboBox::from_label("WAV")
                .selected_text(&self.import_pick)
                .show_ui(ui, |ui| {
                    for name in &self.imports {
                        ui.selectable_value(&mut self.import_pick, name.clone(), name);
                    }
                });
        });
        ui.horizontal(|ui| {
            if ui.button("Load to selected").clicked() {
                let pick = self.import_pick.clone();
                self.load_wav(&pick, false);
            }
            if ui.button("Load as new target").clicked() {
                let pick = self.import_pick.clone();
                self.load_wav(&pick, true);
            }
        });
        if self.recording.is_some() {
            if ui.button("\u{25a0} Stop take (10 s max)").clicked() {
                self.stop_take(false);
            }
        } else if ui.button("\u{25cf} Take from input").clicked() {
            self.start_take();
        }
        if self.last_take.is_some() {
            ui.horizontal(|ui| {
                let end_max = self.trim_end_ms.max(1.0);
                ui.add(egui::Slider::new(&mut self.trim_start_ms, 0.0..=end_max).text("start ms"));
                let start_min = self.trim_start_ms.min(self.trim_end_ms).max(0.0);
                let total = self
                    .last_take
                    .as_ref()
                    .map(|t| t.original.len() as f32 / t.rate as f32 * 1000.0)
                    .unwrap_or(0.0);
                ui.add(
                    egui::Slider::new(&mut self.trim_end_ms, start_min..=total.max(1.0))
                        .text("end ms"),
                );
                if ui.button("Apply trim").clicked() {
                    self.apply_trim();
                }
            });
            ui.horizontal(|ui| {
                if ui.button("To selected target").clicked() {
                    self.assign_take(false);
                }
                if ui.button("As new target").clicked() {
                    self.assign_take(true);
                }
            });
        }
        ui.label(&self.sample_status);
    }

    fn load_wav(&mut self, file: &str, to_new: bool) {
        if file.is_empty() {
            self.sample_status = "no WAV selected — Refresh imports".to_string();
            return;
        }
        let path = format!("imports/{file}");
        let stem = file
            .trim_end_matches(".wav")
            .trim_end_matches(".WAV")
            .to_string();
        match crate::sample::read_wav_mono(&path) {
            Ok((rate, pcm)) => self.take_audio(stem, rate, pcm, to_new),
            Err(e) => self.sample_status = format!("WAV failed: {e}"),
        }
    }

    fn load_factory(&mut self, t: usize, factory: Factory) {
        let inst = match factory {
            Factory::Kit => percussion_kit(&self.pool),
            Factory::Snare => snare9(&self.pool),
            Factory::Pluck => pitched_pluck(&self.pool),
        };
        let pattern = Pattern::new(factory.label(), 1);
        self.song.tracks[t].instrument = Some(inst);
        self.song.tracks[t].pattern = Some(pattern.clone());
        self.song.tracks[t].name = factory.label().to_string();
        self.rebuild(t);
        self.send_to(t, Msg::LoadPattern { pattern });
        self.selected = self.song.tracks[t]
            .instrument
            .as_ref()
            .and_then(|inst| inst.targets.first().map(|t| t.id));
        if self.active == t {
            // keep playing position; only sound changes
        }
    }
}

#[derive(Clone, Copy)]
enum Factory {
    Kit,
    Snare,
    Pluck,
}

impl Factory {
    fn label(self) -> &'static str {
        match self {
            Self::Kit => "Kit",
            Self::Snare => "Snare",
            Self::Pluck => "Pluck",
        }
    }
}

fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[note as usize % 12], note as i16 / 12 - 1)
}

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 760.0]),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "Zearo",
        options,
        Box::new(|_cc| Ok(Box::new(ZearoApp::new()) as Box<dyn eframe::App>)),
    );
}
