//! Universal instrument model.
//!
//! One core for every musical behaviour: an [`Instrument`] holds 1..N
//! independently addressable [`Target`]s with stable [`TargetId`]s. A drum
//! pad bank, a 9-take snare and a multisampled pitched instrument are all
//! the same engine type with different [`Selector`]s — "drum kit" and
//! "keyboard" are musician-facing layouts and presets, not engine types.
//!
//! Patterns (see `pattern.rs`) refer to [`TargetId`], never to pad positions
//! or sample names, so sounds and pad mappings can change freely.

use serde::{Deserialize, Serialize};

use crate::sample::{AssetId, AssetPool};

/// Stable playable identity, local to an instrument. Persisted in patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetId(pub u32);

/// One velocity band with alternate takes. The round-robin counter lives in
/// the engine (prepared state), never here, so instruments stay immutable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VelocityZone {
    pub lo: u8,
    pub hi: u8,
    pub takes: Vec<AssetId>,
}

/// One key range of a pitched multisample, with its own root note and
/// velocity bands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRange {
    pub lo_note: u8,
    pub hi_note: u8,
    pub root_note: u8,
    pub zones: Vec<VelocityZone>,
}

/// How a target picks a source for an incoming note. Explicit small set —
/// not a general rule graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    /// Always this source (one-shot drums, sliced hits).
    OneShot { source: AssetId },
    /// Velocity bands with round-robin takes (multisampled drums).
    Velocity { zones: Vec<VelocityZone> },
    /// Key ranges with velocity bands (pitched multisamples).
    Pitched { ranges: Vec<KeyRange> },
}

/// One independently playable thing: a drum voice, a snare, a whole keyboard.
/// Shared shaping (`level`, `decay`) applies to every take at once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub id: TargetId,
    pub name: String,
    pub selector: Selector,
    /// Shared output level (linear gain).
    pub level: f32,
    /// Shared decay scale: 1.0 = as recorded, <1 shorter, >1 longer.
    pub decay: f32,
    /// Voices in the same choke group cut each other (hi-hats). None by default.
    pub choke_group: Option<u8>,
    /// If true, playback rate follows the incoming note.
    pub pitch_tracking: bool,
    /// Reference note for pitch tracking (MIDI).
    pub root_note: u8,
}

/// What the engine must sound for one trigger. `takes` + `rr_key` select the
/// exact source via [`select_take`]; the engine owns the counters.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHit {
    pub takes: Vec<AssetId>,
    /// (target, zone) key into the engine's preallocated RR counters.
    pub rr_key: (u32, u32),
    /// Velocity response gain.
    pub gain: f32,
    /// Playback rate multiplier (1.0 = as recorded).
    pub rate: f32,
}

/// Deterministic round-robin: cycles 0,1,2,…,0. The counter is engine state.
pub fn select_take(takes: &[AssetId], counter: &mut usize) -> Option<AssetId> {
    if takes.is_empty() {
        return None;
    }
    let take = takes[*counter % takes.len()];
    *counter = counter.wrapping_add(1);
    Some(take)
}

/// Velocity response curve: gentle at the bottom, full at 127.
pub fn velocity_gain(velocity: u8) -> f32 {
    (f32::from(velocity) / 127.0).powf(1.5)
}

fn semitone_rate(note: u8, root: u8) -> f32 {
    2f32.powf((f32::from(note) - f32::from(root)) / 12.0)
}

/// One playable instrument: targets + the assets they reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instrument {
    pub name: String,
    pub targets: Vec<Target>,
    /// Recipes only — PCM regenerates on load (see `sample.rs`).
    pub asset_names: Vec<(AssetId, String)>,
}

impl Instrument {
    pub fn target(&self, id: TargetId) -> Option<&Target> {
        self.targets.iter().find(|t| t.id == id)
    }

    pub fn target_mut(&mut self, id: TargetId) -> Option<&mut Target> {
        self.targets.iter_mut().find(|t| t.id == id)
    }

    /// Resolve a trigger to concrete sources. Velocity 0 (note-off
    /// convention) resolves to nothing.
    pub fn resolve(&self, target: TargetId, note: u8, velocity: u8) -> Option<ResolvedHit> {
        if velocity == 0 {
            return None;
        }
        let t = self.target(target)?;
        let gain = velocity_gain(velocity) * t.level;
        match &t.selector {
            Selector::OneShot { source } => Some(ResolvedHit {
                takes: vec![*source],
                rr_key: (t.id.0, 0),
                gain,
                rate: if t.pitch_tracking {
                    semitone_rate(note, t.root_note)
                } else {
                    1.0
                },
            }),
            Selector::Velocity { zones } => {
                let (zi, zone) = zones
                    .iter()
                    .enumerate()
                    .find(|(_, z)| velocity >= z.lo && velocity <= z.hi)?;
                Some(ResolvedHit {
                    takes: zone.takes.clone(),
                    rr_key: (t.id.0, zi as u32),
                    gain,
                    rate: 1.0,
                })
            }
            Selector::Pitched { ranges } => {
                let (ri, range) = ranges
                    .iter()
                    .enumerate()
                    .find(|(_, r)| note >= r.lo_note && note <= r.hi_note)?;
                let (zi, zone) = range
                    .zones
                    .iter()
                    .enumerate()
                    .find(|(_, z)| velocity >= z.lo && velocity <= z.hi)?;
                Some(ResolvedHit {
                    takes: zone.takes.clone(),
                    rr_key: (t.id.0, (ri * 16 + zi) as u32),
                    gain,
                    rate: semitone_rate(note, range.root_note),
                })
            }
        }
    }

    /// Every source a target can produce (for audition grids like the 3×3
    /// snare view). Order is stable.
    pub fn audition_sources(&self, target: TargetId) -> Vec<AssetId> {
        let Some(t) = self.target(target) else {
            return Vec::new();
        };
        match &t.selector {
            Selector::OneShot { source } => vec![*source],
            Selector::Velocity { zones } => zones.iter().flat_map(|z| z.takes.clone()).collect(),
            Selector::Pitched { ranges } => ranges
                .iter()
                .flat_map(|r| r.zones.iter().flat_map(|z| z.takes.clone()))
                .collect(),
        }
    }

    /// Swap one take for another without disturbing the rest of the
    /// instrument. Returns true if the swap landed.
    pub fn replace_source(&mut self, target: TargetId, old: AssetId, new: AssetId) -> bool {
        let Some(t) = self.target_mut(target) else {
            return false;
        };
        let mut landed = false;
        let mut swap = |takes: &mut Vec<AssetId>| {
            for take in takes.iter_mut() {
                if *take == old {
                    *take = new;
                    landed = true;
                }
            }
        };
        match &mut t.selector {
            Selector::OneShot { source } => {
                if *source == old {
                    *source = new;
                    landed = true;
                }
            }
            Selector::Velocity { zones } => {
                for z in zones {
                    swap(&mut z.takes);
                }
            }
            Selector::Pitched { ranges } => {
                for r in ranges {
                    for z in &mut r.zones {
                        swap(&mut z.takes);
                    }
                }
            }
        }
        landed
    }

    /// Structural sanity: zones cover velocity without gaps/overlaps.
    pub fn validate(&self) -> Result<(), String> {
        for t in &self.targets {
            let check_zones = |zones: &[VelocityZone], ctx: &str| -> Result<(), String> {
                if zones.is_empty() {
                    return Err(format!("{ctx}: no velocity zones"));
                }
                for z in zones {
                    if z.lo > z.hi {
                        return Err(format!("{ctx}: inverted zone {}-{}", z.lo, z.hi));
                    }
                    if z.takes.is_empty() {
                        return Err(format!("{ctx}: zone {}-{} has no takes", z.lo, z.hi));
                    }
                }
                let mut cover = [false; 128];
                for z in zones {
                    for v in z.lo..=z.hi {
                        if cover[v as usize] {
                            return Err(format!("{ctx}: velocity {v} covered twice"));
                        }
                        cover[v as usize] = true;
                    }
                }
                // Velocity 0 is note-off; 1..=127 must all resolve.
                for v in 1..=127u8 {
                    if !cover[v as usize] {
                        return Err(format!("{ctx}: velocity {v} uncovered"));
                    }
                }
                Ok(())
            };
            match &t.selector {
                Selector::OneShot { .. } => {}
                Selector::Velocity { zones } => {
                    check_zones(zones, &format!("target {}", t.name))?;
                }
                Selector::Pitched { ranges } => {
                    if ranges.is_empty() {
                        return Err(format!("target {}: no key ranges", t.name));
                    }
                    for r in ranges {
                        if r.lo_note > r.hi_note {
                            return Err(format!("target {}: inverted key range", t.name));
                        }
                        check_zones(&r.zones, &format!("target {} note {}", t.name, r.root_note))?;
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Prototype factories: the three packet-002 test instruments
// ---------------------------------------------------------------------------

fn zones_3x3(pool: &AssetPool, prefix: &str) -> Vec<VelocityZone> {
    let bands = [(1u8, 50u8, "soft"), (51, 100, "mid"), (101, 127, "hard")];
    bands
        .iter()
        .map(|(lo, hi, name)| {
            let takes = (0..3)
                .map(|t| {
                    pool.find_by_name(&format!("{prefix}_{name}_{t}"))
                        .unwrap_or_else(|| panic!("missing asset {prefix}_{name}_{t}"))
                })
                .collect();
            VelocityZone {
                lo: *lo,
                hi: *hi,
                takes,
            }
        })
        .collect()
}

fn asset_names(pool: &AssetPool) -> Vec<(AssetId, String)> {
    let mut names: Vec<(AssetId, String)> = Vec::new();
    // Record every asset referenced anywhere; the pool is small.
    let mut seen = std::collections::HashSet::new();
    for id in 0..pool.len() as u32 {
        let id = AssetId(id);
        if seen.insert(id.0) {
            if let Some(a) = pool.get(id) {
                names.push((id, a.name.clone()));
            }
        }
    }
    names
}

/// A: small percussion bank — one-shot voices on stable targets.
pub fn percussion_kit(pool: &AssetPool) -> Instrument {
    let need = |name: &str| {
        pool.find_by_name(name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let voice = |id: u32, name: &str, asset: &str, choke: Option<u8>| Target {
        id: TargetId(id),
        name: name.to_string(),
        selector: Selector::OneShot {
            source: need(asset),
        },
        level: 0.9,
        decay: 1.0,
        choke_group: choke,
        pitch_tracking: false,
        root_note: 60,
    };
    Instrument {
        name: "Percussion Kit".to_string(),
        targets: vec![
            voice(0, "Kick", "kick", None),
            voice(1, "Snare", "snare_kit", None),
            voice(2, "Hat Closed", "hat_closed", Some(1)),
            voice(3, "Hat Open", "hat_open", Some(1)),
            voice(4, "Clap", "clap", None),
            voice(5, "Tom Lo", "tom_lo", None),
            voice(6, "Tom Hi", "tom_hi", None),
            voice(7, "Rim", "rim", None),
            voice(8, "Shaker", "shaker", None),
            voice(9, "Crash", "crash", None),
        ],
        asset_names: asset_names(pool),
    }
}

/// B: one target, nine recordings — 3 velocity bands × 3 round-robin takes.
pub fn snare9(pool: &AssetPool) -> Instrument {
    Instrument {
        name: "Nine Snare".to_string(),
        targets: vec![Target {
            id: TargetId(0),
            name: "Snare".to_string(),
            selector: Selector::Velocity {
                zones: zones_3x3(pool, "snare"),
            },
            level: 0.9,
            // One shared decay for all nine takes (acceptance: single control).
            decay: 1.0,
            choke_group: None,
            pitch_tracking: false,
            root_note: 60,
        }],
        asset_names: asset_names(pool),
    }
}

/// C: small pitched multisample — 5 roots × 2 velocity bands, same core.
pub fn pitched_pluck(pool: &AssetPool) -> Instrument {
    let notes = [48u8, 53, 57, 60, 64];
    // Ranges centered between roots.
    let edges = [0u8, 51, 55, 59, 62, 127];
    let ranges = notes
        .iter()
        .enumerate()
        .map(|(i, root)| {
            let zones = [(1u8, 80u8, "soft"), (81, 127, "hard")]
                .iter()
                .map(|(lo, hi, band)| {
                    let takes = vec![pool
                        .find_by_name(&format!("pluck_{root}_{band}"))
                        .unwrap_or_else(|| panic!("missing pluck_{root}_{band}"))];
                    VelocityZone {
                        lo: *lo,
                        hi: *hi,
                        takes,
                    }
                })
                .collect();
            KeyRange {
                lo_note: edges[i],
                hi_note: edges[i + 1],
                root_note: *root,
                zones,
            }
        })
        .collect();
    Instrument {
        name: "Pluck".to_string(),
        targets: vec![Target {
            id: TargetId(0),
            name: "Pluck".to_string(),
            selector: Selector::Pitched { ranges },
            level: 0.85,
            decay: 1.0,
            choke_group: None,
            pitch_tracking: true,
            root_note: 60,
        }],
        asset_names: asset_names(pool),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::AssetPool;

    fn pool() -> AssetPool {
        AssetPool::generate()
    }

    #[test]
    fn all_three_archetypes_validate() {
        let pool = pool();
        for inst in [percussion_kit(&pool), snare9(&pool), pitched_pluck(&pool)] {
            let name = inst.name.clone();
            assert!(inst.validate().is_ok(), "{name}");
        }
    }

    #[test]
    fn velocity_boundaries_select_correct_bands() {
        let pool = pool();
        let inst = snare9(&pool);
        let soft = inst.resolve(TargetId(0), 60, 50).expect("vel 50");
        let mid = inst.resolve(TargetId(0), 60, 51).expect("vel 51");
        let hard = inst.resolve(TargetId(0), 60, 101).expect("vel 101");
        assert_ne!(soft.takes, mid.takes, "50 is soft, 51 is mid");
        assert_ne!(mid.takes, hard.takes, "51 is mid, 101 is hard");
        assert_eq!(soft.takes.len(), 3, "three takes per band");
        assert_eq!(inst.resolve(TargetId(0), 60, 0), None, "vel 0 is note-off");
    }

    #[test]
    fn round_robin_cycles_deterministically() {
        let pool = pool();
        let inst = snare9(&pool);
        let hit = inst.resolve(TargetId(0), 60, 64).expect("mid band");
        let mut counter = 0usize;
        let seq: Vec<_> = (0..7)
            .map(|_| select_take(&hit.takes, &mut counter))
            .collect();
        assert_eq!(
            seq,
            vec![
                Some(hit.takes[0]),
                Some(hit.takes[1]),
                Some(hit.takes[2]),
                Some(hit.takes[0]),
                Some(hit.takes[1]),
                Some(hit.takes[2]),
                Some(hit.takes[0]),
            ]
        );
        assert_eq!(
            select_take(&[], &mut counter),
            None,
            "empty takes resolve to none"
        );
    }

    #[test]
    fn pitched_uses_the_same_core() {
        let pool = pool();
        let inst = pitched_pluck(&pool);
        // Same resolve() as drums; rate follows pitch from the nearest root.
        let c4 = inst.resolve(TargetId(0), 60, 100).expect("C4 hard");
        assert!((c4.rate - 1.0).abs() < 1e-6, "root plays as recorded");
        let d4 = inst.resolve(TargetId(0), 62, 100).expect("D4 hard");
        assert!(
            (d4.rate - 2f32.powf(2.0 / 12.0)).abs() < 1e-5,
            "whole step up retunes"
        );
        // Soft band differs from hard band (different sources).
        let soft = inst.resolve(TargetId(0), 60, 40).expect("C4 soft");
        assert_ne!(soft.takes, c4.takes);
        // One-shot kit resolves through the identical call.
        let kit = percussion_kit(&pool);
        let kick = kit.resolve(TargetId(0), 60, 100).expect("kick");
        assert_eq!(kick.rate, 1.0, "drums ignore pitch");
        assert!(kick.gain > 0.6, "hard hit is loud: {}", kick.gain);
        assert!(kit.resolve(TargetId(0), 60, 10).unwrap().gain < kick.gain);
    }

    #[test]
    fn replace_source_disturbs_nothing_else() {
        let pool = pool();
        let mut inst = snare9(&pool);
        let before = inst.audition_sources(TargetId(0));
        assert_eq!(before.len(), 9, "nine takes to audition");
        let old = before[4];
        let new = pool.find_by_name("rim").expect("rim");
        assert!(inst.replace_source(TargetId(0), old, new));
        let after = inst.audition_sources(TargetId(0));
        assert_eq!(after.len(), 9);
        assert_eq!(after.iter().filter(|&&a| a == new).count(), 1);
        assert!(!after.contains(&old));
        // Shared shaping is a single field for all nine takes.
        inst.target_mut(TargetId(0)).unwrap().decay = 0.5;
        inst.validate().expect("still valid after edit");
        assert!(!inst.replace_source(TargetId(0), AssetId(9999), new));
        assert!(inst.audition_sources(TargetId(4242)).is_empty());
    }

    #[test]
    fn choke_groups_only_where_they_belong() {
        let pool = pool();
        let kit = percussion_kit(&pool);
        let hats: Vec<_> = kit
            .targets
            .iter()
            .filter(|t| t.choke_group == Some(1))
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(hats, vec!["Hat Closed".to_string(), "Hat Open".to_string()]);
        assert!(kit.target(TargetId(0)).unwrap().choke_group.is_none());
    }
}
