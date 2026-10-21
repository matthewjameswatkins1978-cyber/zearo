//! Playing and editing layouts: views, not instrument types.
//!
//! A [`Layout`] maps the 8×8 pad grid (or a step editor row) to musical
//! actions on one shared [`Instrument`] and [`Pattern`]. Switching layouts
//! never copies sounds, replaces presets, rewrites clips or disturbs
//! playback — the layout holds no audio state at all. Some representations
//! are deliberately layout-specific: the 3×3 take grid only makes sense for
//! auditioning, never for recording.

use crate::instrument::{Instrument, TargetId};
use crate::sample::AssetId;

/// The 8×8 hardware grid, row-major: pad = row * 8 + col.
pub const GRID: usize = 64;

/// What a pad press means under the current layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadAction {
    /// Play a target (optionally pitched) at a velocity.
    Hit {
        target: TargetId,
        note: u8,
        velocity: u8,
    },
    /// Audition one concrete recording (never recorded into the pattern).
    Audition { asset: AssetId },
    /// Toggle a step for a target at an exact tick.
    ToggleStep { target: TargetId, tick: u64 },
    /// This pad does nothing in this layout.
    None,
}

/// Musician-facing surfaces over the same engine core.
#[derive(Debug, Clone)]
pub enum Layout {
    /// Pad bank: pad i plays the i-th target in `order`.
    DrumPads { order: Vec<TargetId>, velocity: u8 },
    /// 4 voices × 16 steps across the grid (two pad rows per voice).
    Steps {
        rows: Vec<TargetId>,
        steps: u16,
        pattern_ticks: u64,
    },
    /// Chromatic keyboard into one pitched target.
    Keys {
        target: TargetId,
        base_note: u8,
        velocity: u8,
    },
    /// Event-list editor: pads unused, pattern shown as text/grid rows.
    Notes,
    /// Audition grid: pad i plays the i-th take directly.
    Audition { takes: Vec<AssetId> },
}

impl Layout {
    /// Drum-pad bank following the instrument's own target order.
    pub fn pads_for(instrument: &Instrument, velocity: u8) -> Self {
        Self::DrumPads {
            order: instrument.targets.iter().map(|t| t.id).collect(),
            velocity,
        }
    }

    /// Step grid for up to four targets (extra targets stay playable via
    /// pads — the grid is a view, never a voice cap).
    pub fn steps_for(targets: Vec<TargetId>, pattern_ticks: u64) -> Self {
        Self::Steps {
            rows: targets.into_iter().take(4).collect(),
            steps: 16,
            pattern_ticks,
        }
    }

    /// Take-audition grid for one multisampled target (e.g. the 9-snare).
    pub fn audition_for(instrument: &Instrument, target: TargetId) -> Self {
        Self::Audition {
            takes: instrument.audition_sources(target),
        }
    }

    pub fn pad_action(&self, pad: u8) -> PadAction {
        if pad as usize >= GRID {
            return PadAction::None;
        }
        match self {
            Self::DrumPads { order, velocity } => match order.get(pad as usize) {
                Some(target) => PadAction::Hit {
                    target: *target,
                    note: 60,
                    velocity: *velocity,
                },
                None => PadAction::None,
            },
            Self::Steps {
                rows,
                steps,
                pattern_ticks,
            } => {
                let row = pad / 8;
                let col = pad % 8;
                let voice = (row / 2) as usize;
                if voice >= rows.len() {
                    return PadAction::None;
                }
                let step = ((row % 2) * 8 + col) as u64;
                if step >= *steps as u64 {
                    return PadAction::None;
                }
                PadAction::ToggleStep {
                    target: rows[voice],
                    tick: step * pattern_ticks / *steps as u64,
                }
            }
            Self::Keys {
                target,
                base_note,
                velocity,
            } => {
                let note = base_note.saturating_add(pad);
                PadAction::Hit {
                    target: *target,
                    note,
                    velocity: *velocity,
                }
            }
            Self::Notes => PadAction::None,
            Self::Audition { takes } => match takes.get(pad as usize) {
                Some(asset) => PadAction::Audition { asset: *asset },
                None => PadAction::None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{percussion_kit, pitched_pluck, snare9};
    use crate::pattern::Pattern;
    use crate::sample::AssetPool;

    #[test]
    fn drum_pads_follow_target_order() {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let layout = Layout::pads_for(&inst, 100);
        assert!(matches!(
            layout.pad_action(0),
            PadAction::Hit {
                target: TargetId(0),
                note: 60,
                velocity: 100
            }
        ));
        assert!(matches!(
            layout.pad_action(13),
            PadAction::Hit {
                target: TargetId(13),
                ..
            }
        ));
        assert_eq!(
            layout.pad_action(14),
            PadAction::None,
            "only 14 voices mapped"
        );
        assert_eq!(layout.pad_action(64), PadAction::None, "outside the grid");
    }

    #[test]
    fn step_grid_maps_four_voices_by_sixteen() {
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let order: Vec<TargetId> = inst.targets.iter().take(4).map(|t| t.id).collect();
        let layout = Layout::steps_for(order.clone(), 3840);
        // Voice 0: grid rows 0-1; voice 1: rows 2-3.
        assert_eq!(
            layout.pad_action(0),
            PadAction::ToggleStep {
                target: order[0],
                tick: 0
            }
        );
        assert_eq!(
            layout.pad_action(7),
            PadAction::ToggleStep {
                target: order[0],
                tick: 7 * 240
            }
        );
        assert_eq!(
            layout.pad_action(8),
            PadAction::ToggleStep {
                target: order[0],
                tick: 8 * 240
            }
        );
        assert_eq!(
            layout.pad_action(16),
            PadAction::ToggleStep {
                target: order[1],
                tick: 0
            }
        );
        assert_eq!(
            layout.pad_action(63),
            PadAction::ToggleStep {
                target: order[3],
                tick: 15 * 240
            }
        );
    }

    #[test]
    fn keyboard_is_chromatic_into_one_target() {
        let pool = AssetPool::generate();
        let inst = pitched_pluck(&pool);
        let layout = Layout::Keys {
            target: TargetId(0),
            base_note: 48,
            velocity: 90,
        };
        assert_eq!(
            layout.pad_action(0),
            PadAction::Hit {
                target: TargetId(0),
                note: 48,
                velocity: 90
            }
        );
        assert_eq!(
            layout.pad_action(12),
            PadAction::Hit {
                target: TargetId(0),
                note: 60,
                velocity: 90
            }
        );
        let _ = inst;
    }

    #[test]
    fn audition_grid_exposes_every_take() {
        let pool = AssetPool::generate();
        let inst = snare9(&pool);
        let layout = Layout::audition_for(&inst, TargetId(0));
        let takes = inst.audition_sources(TargetId(0));
        assert_eq!(takes.len(), 9);
        for (i, take) in takes.iter().enumerate() {
            assert_eq!(
                layout.pad_action(i as u8),
                PadAction::Audition { asset: *take }
            );
        }
        assert_eq!(layout.pad_action(9), PadAction::None);
    }

    #[test]
    fn switching_layouts_preserves_music() {
        // Acceptance: layout switching leaves audio and musical state intact.
        // Layouts borrow the same instrument + pattern; switching replaces
        // only the view value.
        let pool = AssetPool::generate();
        let inst = percussion_kit(&pool);
        let mut pattern = Pattern::new("beat", 1);
        pattern.add(TargetId(0), 60, 100, 0);
        let before: Vec<(u32, u8, u64)> = pattern
            .events
            .iter()
            .map(|e| (e.target.0, e.velocity, e.tick))
            .collect();

        let pads = Layout::pads_for(&inst, 100);
        let steps = Layout::steps_for(inst.targets.iter().map(|t| t.id).collect(), 3840);
        let keys = Layout::Keys {
            target: TargetId(0),
            base_note: 36,
            velocity: 100,
        };
        // Play the same pad under two layouts: both address stable targets
        // that resolve through the same instrument.
        let a = pads.pad_action(2);
        let b = steps.pad_action(32);
        assert!(matches!(
            a,
            PadAction::Hit {
                target: TargetId(2),
                ..
            }
        ));
        assert!(matches!(
            b,
            PadAction::ToggleStep {
                target: TargetId(2),
                ..
            }
        ));
        // Step (4,0) toggles target 2 — the pad target and the step target
        // are the same stable identity, so the pattern survives a pad
        // reordering that keeps ids.
        let reordered = Layout::DrumPads {
            order: vec![TargetId(2), TargetId(0), TargetId(1)],
            velocity: 100,
        };
        assert!(matches!(
            reordered.pad_action(0),
            PadAction::Hit {
                target: TargetId(2),
                ..
            }
        ));
        let after: Vec<(u32, u8, u64)> = pattern
            .events
            .iter()
            .map(|e| (e.target.0, e.velocity, e.tick))
            .collect();
        assert_eq!(before, after, "layouts never rewrite the pattern");
        let _ = keys;
    }
}
