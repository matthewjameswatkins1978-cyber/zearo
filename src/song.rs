//! Song model and versioned save format.
//!
//! A [`Song`] is eight tracks; each track optionally hosts an [`Instrument`]
//! and a [`Pattern`]. The document stores instrument *definitions* (recipes
//! referenced by stable [`AssetId`]s regenerate bit-identically on load) and
//! patterns verbatim. Format identity and version are checked on load so an
//! old build fails loudly instead of misreading a newer document.

use serde::{Deserialize, Serialize};

use crate::instrument::Instrument;
use crate::pattern::Pattern;

/// Envelope tag for every Zearo document.
pub const DOC_FORMAT: &str = "zearo-doc";
/// Current document version. Bump on any breaking model change and add a
/// migration; never silently accept an unknown version.
pub const DOC_VERSION: u32 = 1;
/// Fixed song size for this milestone (arranger depth comes later).
pub const TRACKS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub instrument: Option<Instrument>,
    pub pattern: Option<Pattern>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Song {
    pub name: String,
    pub bpm: f64,
    pub tracks: Vec<Track>,
    /// Original user recordings (WAV imports, input takes). The pool only
    /// caches prepared playback buffers; these originals are the source
    /// of truth for re-trimming and reloads.
    #[serde(default)]
    pub samples: Vec<StoredSample>,
}

/// One stored original recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSample {
    pub name: String,
    pub sample_rate: u32,
    pub pcm: Vec<f32>,
}

impl Song {
    pub fn new(name: &str, bpm: f64) -> Self {
        Self {
            name: name.to_string(),
            bpm,
            tracks: (0..TRACKS)
                .map(|i| Track {
                    name: format!("Track {}", i + 1),
                    instrument: None,
                    pattern: None,
                })
                .collect(),
            samples: Vec::new(),
        }
    }

    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn track_mut(&mut self, index: usize) -> Option<&mut Track> {
        self.tracks.get_mut(index)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    song: serde_json::Value,
}

/// Version-0 song shape (no stored samples). Migrated on load.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SongV0 {
    name: String,
    bpm: f64,
    tracks: Vec<Track>,
}

/// Restore a song's originals into a fresh pool. Import order is the doc
/// order, so restored ids match the takes that reference them.
/// Call on a pool built by `AssetPool::generate` (or rebuilt for it).
pub fn restore_samples(
    pool: &mut crate::sample::AssetPool,
    song: &Song,
) -> Vec<crate::sample::AssetId> {
    let mut ids = Vec::new();
    for s in &song.samples {
        if let Some(existing) = pool.find_by_name(&s.name) {
            ids.push(existing);
        } else {
            ids.push(pool.import(&s.name, s.sample_rate, s.pcm.clone()));
        }
    }
    ids
}

#[derive(Debug)]
pub enum LoadError {
    Json(String),
    Format(String),
    Version(u32),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "invalid JSON: {e}"),
            Self::Format(e) => write!(f, "not a Zearo document (format tag {e:?})"),
            Self::Version(v) => write!(f, "unsupported document version {v}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Serialize a song to the experimental document format.
pub fn save_song(song: &Song) -> Result<String, serde_json::Error> {
    let envelope = Envelope {
        format: DOC_FORMAT.to_string(),
        version: DOC_VERSION,
        song: serde_json::to_value(song.clone())?,
    };
    serde_json::to_string_pretty(&envelope)
}

/// Load and validate a song document, migrating older versions forward.
/// Unknown future versions are rejected, never guessed.
pub fn load_song(text: &str) -> Result<Song, LoadError> {
    let envelope: Envelope =
        serde_json::from_str(text).map_err(|e| LoadError::Json(e.to_string()))?;
    if envelope.format != DOC_FORMAT {
        return Err(LoadError::Format(envelope.format));
    }
    match envelope.version {
        DOC_VERSION => {
            serde_json::from_value(envelope.song).map_err(|e| LoadError::Json(e.to_string()))
        }
        0 => {
            let old: SongV0 = serde_json::from_value(envelope.song)
                .map_err(|e| LoadError::Json(e.to_string()))?;
            Ok(Song {
                name: old.name,
                bpm: old.bpm,
                tracks: old.tracks,
                samples: Vec::new(),
            })
        }
        v => Err(LoadError::Version(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::prepare;
    use crate::instrument::{percussion_kit, TargetId};
    use crate::sample::AssetPool;
    use std::sync::Arc;

    fn demo_song() -> Song {
        let mut song = Song::new("demo", 120.0);
        song.tracks[0].name = "Drums".to_string();
        song.tracks[0].instrument = Some(percussion_kit(&AssetPool::generate()));
        let mut pattern = Pattern::new("beat", 1);
        pattern.add(TargetId(0), 60, 100, 0);
        pattern.add(TargetId(1), 60, 90, 240);
        pattern.add(TargetId(2), 60, 80, 480);
        song.tracks[0].pattern = Some(pattern);
        song
    }

    #[test]
    fn save_reload_preserves_state() {
        let song = demo_song();
        let text = save_song(&song).expect("saves");
        let back = load_song(&text).expect("loads");
        assert_eq!(back.tracks.len(), TRACKS);
        assert_eq!(back.bpm, 120.0);
        let pattern = back.tracks[0].pattern.as_ref().expect("pattern");
        assert_eq!(pattern.events.len(), 3);
        assert_eq!(pattern.events[1].tick, 240);
        // The reloaded instrument still prepares and validates.
        let pool = AssetPool::generate();
        let inst = back.tracks[0].instrument.as_ref().expect("instrument");
        inst.validate().expect("valid after reload");
        prepare(inst, &pool, 1).expect("prepares after reload");
    }

    #[test]
    fn save_reload_renders_identically() {
        use crate::engine::Engine;
        let pool = AssetPool::generate();
        let song = demo_song();
        let text = save_song(&song).expect("saves");
        let back = load_song(&text).expect("loads");
        let render = |song: &Song| -> Vec<f32> {
            let track = &song.tracks[0];
            let prepared = Arc::new(prepare(track.instrument.as_ref().unwrap(), &pool, 1).unwrap());
            let (mut engine, _rx) = Engine::new(prepared, 44100.0);
            for e in &track.pattern.as_ref().unwrap().events {
                assert!(engine.trigger(e.target, e.note, e.velocity));
            }
            let mut l = vec![0.0; 30_000];
            let mut r = vec![0.0; 30_000];
            engine.render_block(&mut l, &mut r);
            l
        };
        assert_eq!(
            render(&song),
            render(&back),
            "bit-identical render after reload"
        );
    }

    #[test]
    fn rejects_foreign_and_future_documents() {
        assert!(matches!(load_song("{}"), Err(LoadError::Json(_))));
        assert!(matches!(
            load_song(
                r#"{"format":"other","version":0,"song":{"name":"","bpm":120.0,"tracks":[]}}"#
            ),
            Err(LoadError::Format(_))
        ));
        let future = format!(
            r#"{{"format":"{DOC_FORMAT}","version":99,"song":{{"name":"","bpm":120.0,"tracks":[]}}}}"#
        );
        assert!(matches!(load_song(&future), Err(LoadError::Version(99))));
    }

    #[test]
    fn v0_documents_migrate_forward() {
        let v0 = format!(
            r#"{{"format":"{DOC_FORMAT}","version":0,"song":{{"name":"old","bpm":100.0,"tracks":[]}}}}"#
        );
        let back = load_song(&v0).expect("v0 migrates");
        assert_eq!(back.name, "old");
        assert!(back.samples.is_empty());
    }

    #[test]
    fn imported_take_round_trips_with_samples() {
        use crate::engine::Engine;
        use crate::sample::{prepare_take, SAMPLE_RATE};
        // Simulate a user take: loud mid section with quiet edges.
        let mut raw = vec![0.0f32; SAMPLE_RATE as usize];
        for (i, s) in raw
            .iter_mut()
            .enumerate()
            .take(SAMPLE_RATE as usize / 2)
            .skip(1000)
        {
            let t = i as f32 / SAMPLE_RATE as f32;
            *s = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.4;
        }
        let prep = prepare_take(SAMPLE_RATE, &raw);
        assert!(prep.peak > 0.5);
        // Fresh pool + instrument take referencing the import.
        let mut pool = AssetPool::generate();
        let id = pool.import("user-take", SAMPLE_RATE, prep.pcm.clone());
        let mut song = demo_song();
        song.tracks[0].instrument.as_mut().unwrap().targets[0] = crate::instrument::Target {
            id: crate::instrument::TargetId(40),
            name: "User Take".to_string(),
            selector: crate::instrument::Selector::OneShot { source: id },
            level: 0.9,
            decay: 1.0,
            choke_group: None,
            pitch_tracking: false,
            root_note: 60,
        };
        song.tracks[0].pattern.as_mut().unwrap().events.clear();
        song.tracks[0]
            .pattern
            .as_mut()
            .unwrap()
            .add(crate::instrument::TargetId(40), 60, 100, 0);
        song.samples.push(StoredSample {
            name: "user-take".to_string(),
            sample_rate: SAMPLE_RATE,
            pcm: raw,
        });
        let text = save_song(&song).expect("saves");
        // Fresh session: regenerate, restore, load, play.
        let mut pool2 = AssetPool::generate();
        let back = load_song(&text).expect("loads v1");
        assert_eq!(back.samples.len(), 1);
        let ids = restore_samples(&mut pool2, &back);
        assert_eq!(ids, vec![id], "stable ids across restore");
        let inst = back.tracks[0].instrument.as_ref().unwrap();
        inst.validate().expect("valid");
        let prepared = Arc::new(prepare(inst, &pool2, 1).expect("prepares with import"));
        let (mut engine, _rx) = Engine::new(prepared, 44100.0);
        assert!(engine.trigger(crate::instrument::TargetId(40), 60, 100));
        let mut l = vec![0.0; 5000];
        let mut r = vec![0.0; 5000];
        engine.render_block(&mut l, &mut r);
        assert!(l.iter().any(|s| s.abs() > 0.05), "imported take sounds");
        // Idempotent restore: loading twice does not duplicate.
        let ids2 = restore_samples(&mut pool2, &back);
        assert_eq!(ids, ids2);
        assert_eq!(pool2.find_by_name("user-take"), Some(id));
    }

    #[test]
    fn pattern_survives_sound_swap() {
        // Acceptance: swapping sounds never rewrites the music.
        let pool = AssetPool::generate();
        let mut song = demo_song();
        let before: Vec<(u32, u64)> = song.tracks[0]
            .pattern
            .as_ref()
            .unwrap()
            .events
            .iter()
            .map(|e| (e.target.0, e.tick))
            .collect();
        // Energy of the original kit, so the swap must audibly differ.
        let render_energy = |song: &Song| -> f32 {
            use crate::engine::Engine;
            let track = &song.tracks[0];
            let prepared = Arc::new(prepare(track.instrument.as_ref().unwrap(), &pool, 1).unwrap());
            let (mut engine, _rx) = Engine::new(prepared, 44100.0);
            for e in &track.pattern.as_ref().unwrap().events {
                engine.trigger(e.target, e.note, e.velocity);
            }
            let mut l = vec![0.0; 30_000];
            let mut r = vec![0.0; 30_000];
            engine.render_block(&mut l, &mut r);
            l.iter().map(|s| s * s).sum()
        };
        let original_e = render_energy(&song);
        // Swap the snare for a rim and the kit kick for its alternate.
        let inst = song.tracks[0].instrument.as_mut().unwrap();
        let rim = pool.find_by_name("rim").unwrap();
        let snare = pool.find_by_name("snare_kit").unwrap();
        assert!(inst.replace_source(TargetId(1), snare, rim));
        let kick = pool.find_by_name("kick").unwrap();
        let kick_alt = pool.find_by_name("kick_alt").unwrap();
        assert!(inst.replace_source(TargetId(0), kick, kick_alt));
        let text = save_song(&song).expect("saves");
        let back = load_song(&text).expect("loads");
        let after: Vec<(u32, u64)> = back.tracks[0]
            .pattern
            .as_ref()
            .unwrap()
            .events
            .iter()
            .map(|e| (e.target.0, e.tick))
            .collect();
        assert_eq!(before, after, "events untouched by the sound swap");
        // Same rhythm still renders after the swap and reload: the swapped
        // version is non-silent with an identical event stream.
        let after_e = render_energy(&back);
        assert!(original_e > 0.1 && after_e > 0.1, "both versions sound");
        assert!(
            (original_e - after_e).abs() > 1e-6,
            "swapped sounds actually differ"
        );
    }
}
