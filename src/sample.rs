//! Deterministic generated audio for Zearo instruments.
//!
//! All prototype sounds are synthesized in code from [`Recipe`]s: no
//! third-party samples, no licensing questions, and every take is
//! bit-identical across runs. Save files store recipes, not PCM; the pool
//! regenerates them on load.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Fixed rendering rate for generated assets (Hz).
pub const SAMPLE_RATE: u32 = 44_100;

/// Stable handle for a generated asset. Stored in patterns and instruments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub u32);

/// Deterministic generator identity. This is what gets saved, not the PCM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recipe {
    Kick {
        variant: u32,
    },
    Snare {
        band: u8,
        take: u8,
    },
    Hat {
        open: bool,
        variant: u32,
    },
    Clap {
        variant: u32,
    },
    Tom {
        high: bool,
        variant: u32,
    },
    Rim {
        variant: u32,
    },
    Shaker {
        variant: u32,
    },
    Crash {
        variant: u32,
    },
    Conga {
        variant: u32,
    },
    Cowbell {
        variant: u32,
    },
    Tambourine {
        variant: u32,
    },
    Woodblock {
        high: bool,
        variant: u32,
    },
    /// Pitched pluck recorded at `note` (MIDI), `band` 0 = soft/dark, 1 = hard/bright.
    Pluck {
        note: u8,
        band: u8,
    },
}

/// One immutable generated sound.
pub struct Asset {
    pub id: AssetId,
    pub name: String,
    pub recipe: Recipe,
    pub pcm: Arc<Vec<f32>>,
}

/// Named pool of all prototype assets.
pub struct AssetPool {
    assets: Vec<Asset>,
}

impl AssetPool {
    /// Render every prototype asset. Fast (a few hundred thousand samples)
    /// and safe to run on a background thread at startup.
    pub fn generate() -> Self {
        let mut pool = Self { assets: Vec::new() };
        let mut id = 0u32;
        let mut push = |name: &str, recipe: Recipe| {
            let pcm = Arc::new(render_recipe(&recipe));
            pool.assets.push(Asset {
                id: AssetId(id),
                name: name.to_string(),
                recipe,
                pcm,
            });
            id += 1;
        };

        push("kick", Recipe::Kick { variant: 0 });
        push("kick_alt", Recipe::Kick { variant: 1 });
        for band in 0..3u8 {
            for take in 0..3u8 {
                push(
                    &format!("snare_{}_{}", ["soft", "mid", "hard"][band as usize], take),
                    Recipe::Snare { band, take },
                );
            }
        }
        push("snare_kit", Recipe::Snare { band: 1, take: 0 });
        push(
            "hat_closed",
            Recipe::Hat {
                open: false,
                variant: 0,
            },
        );
        push(
            "hat_open",
            Recipe::Hat {
                open: true,
                variant: 0,
            },
        );
        push("clap", Recipe::Clap { variant: 0 });
        push(
            "tom_lo",
            Recipe::Tom {
                high: false,
                variant: 0,
            },
        );
        push(
            "tom_hi",
            Recipe::Tom {
                high: true,
                variant: 0,
            },
        );
        push("rim", Recipe::Rim { variant: 0 });
        push("shaker", Recipe::Shaker { variant: 0 });
        push("crash", Recipe::Crash { variant: 0 });
        push("conga", Recipe::Conga { variant: 0 });
        push("cowbell", Recipe::Cowbell { variant: 0 });
        push("tambourine", Recipe::Tambourine { variant: 0 });
        push(
            "woodblock_hi",
            Recipe::Woodblock {
                high: true,
                variant: 0,
            },
        );
        push(
            "woodblock_lo",
            Recipe::Woodblock {
                high: false,
                variant: 0,
            },
        );
        for note in [36u8, 41, 48, 53, 57, 60, 64] {
            for band in 0..2u8 {
                push(
                    &format!("pluck_{}_{}", note, ["soft", "hard"][band as usize]),
                    Recipe::Pluck { note, band },
                );
            }
        }
        pool
    }

    pub fn get(&self, id: AssetId) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == id)
    }

    pub fn find_by_name(&self, name: &str) -> Option<AssetId> {
        self.assets.iter().find(|a| a.name == name).map(|a| a.id)
    }

    pub fn len(&self) -> usize {
        self.assets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Deterministic RNG (xorshift64*, no external dependency)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // SplitMix64 finalizer: avalanches adjacent seeds (e.g. take 0/1)
        // into unrelated states. Bijective, so distinct seeds stay distinct.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Self(if z == 0 { 0x2545_F491_4F6C_DD1D } else { z })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 32) as f32) / (u32::MAX as f32)
    }

    /// Uniform in [-1, 1).
    fn bipolar(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }
}

// ---------------------------------------------------------------------------
// Synthesis helpers
// ---------------------------------------------------------------------------

fn midi_hz(note: u8) -> f32 {
    440.0 * 2f32.powf((f32::from(note) - 69.0) / 12.0)
}

/// Short safety fades so generated assets never click at their boundaries.
/// The fade-in is 0.7 ms: inaudible, but it removes a step discontinuity.
fn apply_fades(buf: &mut [f32]) {
    let fade_in = (SAMPLE_RATE as f32 * 0.0007) as usize;
    let fade_out = (SAMPLE_RATE as f32 * 0.004).min(buf.len() as f32 * 0.25) as usize;
    for (i, s) in buf.iter_mut().enumerate().take(fade_in) {
        *s *= i as f32 / fade_in as f32;
    }
    let n = buf.len();
    for (i, s) in buf.iter_mut().rev().enumerate().take(fade_out) {
        *s *= i as f32 / fade_out as f32;
    }
    let _ = n;
}

fn one_pole_lowpass(buf: &mut [f32], cutoff_hz: f32) {
    let a = 1.0 - (-2.0 * std::f32::consts::PI * cutoff_hz / SAMPLE_RATE as f32).exp();
    let mut y = 0.0f32;
    for s in buf.iter_mut() {
        y += a * (*s - y);
        *s = y;
    }
}

fn one_pole_highpass(buf: &mut [f32], cutoff_hz: f32) {
    let a = (-2.0 * std::f32::consts::PI * cutoff_hz / SAMPLE_RATE as f32).exp();
    let mut y = 0.0f32;
    let mut prev = 0.0f32;
    for s in buf.iter_mut() {
        let x = *s;
        y = a * (y + x - prev);
        prev = x;
        *s = y;
    }
}

/// Render one recipe to mono PCM. Pure function of the recipe.
pub fn render_recipe(recipe: &Recipe) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let mut rng = Rng::new(match recipe {
        Recipe::Kick { variant } => 0x1000 + u64::from(*variant),
        Recipe::Snare { band, take } => 0x2000 + u64::from(*band) * 16 + u64::from(*take),
        Recipe::Hat { open, variant } => 0x3000 + u64::from(*variant) * 2 + u64::from(*open),
        Recipe::Clap { variant } => 0x4000 + u64::from(*variant),
        Recipe::Tom { high, variant } => 0x5000 + u64::from(*variant) * 2 + u64::from(*high),
        Recipe::Rim { variant } => 0x6000 + u64::from(*variant),
        Recipe::Shaker { variant } => 0x7000 + u64::from(*variant),
        Recipe::Crash { variant } => 0x8000 + u64::from(*variant),
        Recipe::Conga { variant } => 0xA000 + u64::from(*variant),
        Recipe::Cowbell { variant } => 0xB000 + u64::from(*variant),
        Recipe::Tambourine { variant } => 0xC000 + u64::from(*variant),
        Recipe::Woodblock { high, variant } => 0xD000 + u64::from(*variant) * 2 + u64::from(*high),
        Recipe::Pluck { note, band } => 0x9000 + u64::from(*note) * 4 + u64::from(*band),
    });

    let mut out = match recipe {
        Recipe::Kick { variant } => {
            let dur = 0.5;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            let sweep = if *variant == 0 { 1.0 } else { 1.12 };
            let mut phase = 0.0f32;
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let freq = (45.0 + 130.0 * (-t * 22.0).exp()) * sweep;
                phase += 2.0 * std::f32::consts::PI * freq / sr;
                let body = phase.sin() * (-t * 9.0).exp();
                let click = if i < 64 {
                    rng.bipolar() * 0.5 * (-t * 300.0).exp()
                } else {
                    0.0
                };
                *s = body * 0.9 + click;
            }
            one_pole_lowpass(&mut buf, 4000.0);
            buf
        }
        Recipe::Snare { band, take } => {
            let dur = 0.35;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            let _ = take;
            let brightness = [0.55, 0.8, 1.0][(*band).min(2) as usize];
            let body_freq = [178.0, 190.0, 203.0][(*band).min(2) as usize];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let noise = rng.bipolar() * (-t * 28.0).exp() * 0.6 * brightness;
                let body =
                    (2.0 * std::f32::consts::PI * body_freq * t).sin() * (-t * 22.0).exp() * 0.7;
                *s = (noise + body) * 1.6;
            }
            one_pole_highpass(&mut buf, 900.0);
            buf
        }
        Recipe::Hat { open, .. } => {
            let dur = if *open { 0.45 } else { 0.09 };
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            let decay = if *open { 9.0 } else { 70.0 };
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                *s = rng.bipolar() * (-t * decay).exp() * 1.5;
            }
            one_pole_highpass(&mut buf, 7000.0);
            buf
        }
        Recipe::Clap { .. } => {
            let dur = 0.28;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                // Three clustered bursts + tail. The minus must negate the
                // exponent argument (decaying bursts), never the exp result
                // (which would clamp everything to silence).
                let burst = ((0.005 - t) * 900.0).exp()
                    + ((0.02 - t) * 700.0).exp()
                    + ((0.045 - t) * 260.0).exp() * 0.7;
                *s = rng.bipolar() * burst.min(1.2) * 0.45;
            }
            one_pole_highpass(&mut buf, 1200.0);
            buf
        }
        Recipe::Tom { high, .. } => {
            let dur = 0.45;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            let (start, end) = if *high { (170.0, 105.0) } else { (115.0, 62.0) };
            let mut phase = 0.0f32;
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let k = (t / (dur as f32)).min(1.0);
                let freq = start + (end - start) * k;
                phase += 2.0 * std::f32::consts::PI * freq / sr;
                *s = phase.sin() * (-t * 8.0).exp() * 0.85
                    + rng.bipolar() * 0.06 * (-t * 60.0).exp();
            }
            buf
        }
        Recipe::Rim { .. } => {
            let dur = 0.07;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                *s = ((2.0 * std::f32::consts::PI * 920.0 * t).sin() * 0.6
                    + (2.0 * std::f32::consts::PI * 1840.0 * t).sin() * 0.3)
                    * (-t * 90.0).exp()
                    * 0.6;
            }
            buf
        }
        Recipe::Shaker { .. } => {
            let dur = 0.16;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                // Two hits + tail. Same precedence note as clap: negate
                // the exponent argument, not the exp() result.
                let env =
                    (((0.01 - t) * 160.0).exp() + ((0.07 - t) * 120.0).exp() * 0.7).clamp(0.0, 1.5);
                *s = rng.bipolar() * env * 0.4;
            }
            one_pole_highpass(&mut buf, 6000.0);
            buf
        }
        Recipe::Crash { .. } => {
            let dur = 1.4;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let mut v = rng.bipolar() * (-t * 4.5).exp() * 1.2;
                for partial in 1..5 {
                    v += (2.0 * std::f32::consts::PI * 620.0 * partial as f32 * 1.31 * t).sin()
                        * (-t * 5.0).exp()
                        * 0.06
                        / partial as f32;
                }
                *s = v;
            }
            one_pole_highpass(&mut buf, 4500.0);
            buf
        }
        Recipe::Conga { .. } => {
            let dur = 0.4;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            let mut phase = 0.0f32;
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let k = (t / (dur as f32)).min(1.0);
                let freq = 215.0 + (130.0 - 215.0) * k;
                phase += 2.0 * std::f32::consts::PI * freq / sr;
                let tone = phase.sin() * (-t * 10.0).exp() * 0.8
                    + (phase * 2.0).sin() * (-t * 14.0).exp() * 0.25;
                let slap = if i < 200 {
                    rng.bipolar() * (-t * 120.0).exp() * 0.5
                } else {
                    0.0
                };
                *s = tone + slap;
            }
            one_pole_lowpass(&mut buf, 5000.0);
            buf
        }
        Recipe::Cowbell { .. } => {
            let dur = 0.35;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let a = (2.0 * std::f32::consts::PI * 540.0 * t).sin();
                let b = (2.0 * std::f32::consts::PI * 810.0 * t).sin();
                let v = (a + b) * 0.9;
                let squared = v / (1.0 + v.abs());
                *s = squared * (-t * 11.0).exp() * 1.3;
            }
            one_pole_highpass(&mut buf, 2500.0);
            buf
        }
        Recipe::Tambourine { .. } => {
            let dur = 0.3;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let jingle = (2.0 * std::f32::consts::PI * 6300.0 * t).sin() * 0.3
                    + (2.0 * std::f32::consts::PI * 8400.0 * t).sin() * 0.2
                    + (2.0 * std::f32::consts::PI * 5200.0 * t).sin() * 0.25;
                let trem = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 28.0 * t).sin();
                *s = (rng.bipolar() * 0.6 + jingle * trem) * (-t * 13.0).exp() * 0.8;
            }
            one_pole_highpass(&mut buf, 6500.0);
            buf
        }
        Recipe::Woodblock { high, .. } => {
            let dur = 0.1;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            let freq = if *high { 880.0 } else { 660.0 };
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                *s = ((2.0 * std::f32::consts::PI * freq * t).sin()
                    + (2.0 * std::f32::consts::PI * freq * 3.01 * t).sin() * 0.3)
                    * (-t * 55.0).exp()
                    * 0.75;
            }
            buf
        }
        Recipe::Pluck { note, band } => {
            let dur = 1.4;
            let n = (dur * f64::from(sr)) as usize;
            let mut buf = vec![0.0f32; n];
            let freq = midi_hz(*note);
            let bright = if *band == 0 { 0.5 } else { 1.0 };
            for (i, s) in buf.iter_mut().enumerate() {
                let t = i as f32 / sr;
                let mut v = 0.0f32;
                for harmonic in 1..7 {
                    let amp = 1.0 / (harmonic as f32).powf(1.0 + 0.6 * (1.0 - bright));
                    v += (2.0 * std::f32::consts::PI * freq * harmonic as f32 * t).sin()
                        * amp
                        * (-t * (2.2 + harmonic as f32 * 1.1)).exp();
                }
                let out_gain = if *band == 0 { 0.33 } else { 0.31 };
                *s = v * out_gain * (0.7 + 0.3 * bright);
            }
            buf
        }
    };
    apply_fades(&mut out);
    out
}

/// Root-mean-square level.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// Zero-crossing rate: cheap timbral fingerprint for distinctness tests.
pub fn zero_crossing_rate(samples: &[f32]) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }
    let mut crossings = 0u32;
    for pair in samples.windows(2) {
        if (pair[0] <= 0.0) != (pair[1] <= 0.0) {
            crossings += 1;
        }
    }
    crossings as f32 / samples.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash_pcm(pcm: &[f32]) -> u64 {
        let mut h = DefaultHasher::new();
        for s in pcm {
            s.to_bits().hash(&mut h);
        }
        h.finish()
    }

    #[test]
    fn prototype_levels_sit_in_a_musical_window() {
        // Every stock sound must be clearly audible but never clip: peak in
        // [0.3, 1.2]. Failures print the offender for recipe tuning.
        let pool = AssetPool::generate();
        let mut bad = Vec::new();
        let mut table = Vec::new();
        for asset in &pool.assets {
            let peak = asset.pcm.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            table.push(format!(
                "{} peak {:.3} rms {:.3}",
                asset.name,
                peak,
                rms(&asset.pcm)
            ));
            if !(0.3..=1.2).contains(&peak) {
                bad.push(asset.name.clone());
            }
        }
        assert!(
            bad.is_empty(),
            "out of window: {bad:?}\n{}",
            table.join("\n")
        );
    }

    #[test]
    fn generation_is_deterministic() {
        let pool = AssetPool::generate();
        for asset in &pool.assets {
            let again = render_recipe(&asset.recipe);
            assert_eq!(hash_pcm(&asset.pcm), hash_pcm(&again), "{}", asset.name);
        }
    }

    #[test]
    fn assets_are_finite_and_bounded() {
        let pool = AssetPool::generate();
        assert!(pool.len() >= 30, "pool holds the full prototype set");
        for asset in &pool.assets {
            assert!(!asset.pcm.is_empty(), "{}", asset.name);
            assert!(
                asset.pcm.iter().all(|s| s.is_finite()),
                "{} has no NaN/inf",
                asset.name
            );
            assert!(
                asset.pcm.iter().all(|s| s.abs() <= 1.5),
                "{} within sane bounds",
                asset.name
            );
            assert!(
                asset.pcm[0].abs() < 0.05 && asset.pcm[asset.pcm.len() - 1].abs() < 0.01,
                "{} starts/ends near silence (no clicks)",
                asset.name
            );
        }
    }

    #[test]
    fn test_sounds_are_demonstrably_different() {
        let pool = AssetPool::generate();
        let get = |name: &str| {
            let id = pool.find_by_name(name).expect(name);
            pool.get(id).unwrap()
        };
        // Kick is low and loud; hat is bright and quiet; crash is long.
        let kick = get("kick");
        let hat = get("hat_closed");
        let crash = get("crash");
        let pluck = get("pluck_60_soft");
        assert!(rms(&kick.pcm) > rms(&hat.pcm) * 1.5, "kick vs hat level");
        assert!(
            zero_crossing_rate(&hat.pcm) > zero_crossing_rate(&kick.pcm) * 3.0,
            "hat vs kick brightness"
        );
        assert!(crash.pcm.len() > kick.pcm.len() * 2, "crash vs kick length");
        assert!(
            (rms(&pluck.pcm) - rms(&kick.pcm)).abs() > 0.01
                || (zero_crossing_rate(&pluck.pcm) - zero_crossing_rate(&kick.pcm)).abs() > 0.005,
            "pluck differs from kick"
        );
    }

    #[test]
    fn snare_takes_differ_but_share_shape() {
        let pool = AssetPool::generate();
        let takes: Vec<_> = (0..3)
            .map(|t| {
                let id = pool
                    .find_by_name(&format!("snare_mid_{t}"))
                    .expect("snare mid take");
                pool.get(id).unwrap().pcm.clone()
            })
            .collect();
        // Same length (shared shape), different content (alternate recordings).
        assert_eq!(takes[0].len(), takes[1].len());
        assert_ne!(hash_pcm(&takes[0]), hash_pcm(&takes[1]));
        assert_ne!(hash_pcm(&takes[1]), hash_pcm(&takes[2]));
        // Same loudness family.
        for t in &takes {
            assert!(
                (rms(t) - rms(&takes[0])).abs() < 0.05,
                "takes match in level"
            );
        }
    }
}
