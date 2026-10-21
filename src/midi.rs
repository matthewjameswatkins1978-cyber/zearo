//! MIDI controller input (Arturia KeyStep and friends).
//!
//! Threading contract: the midir callback runs on a driver thread and only
//! forwards raw bytes over `std::mpsc`. Parsing, mapping and Msg emission
//! all happen on the UI thread, so the single-producer rtrb audio queue is
//! never touched from two threads. No audio-thread work happens here at all.

use std::sync::mpsc::{self, Receiver, Sender};

use crate::instrument::{Instrument, TargetId};

/// Parsed musical MIDI event. Everything else is ignored upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiEvent {
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8 },
}

/// Parse one complete MIDI message. midir delivers reassembled messages
/// (no running-status handling needed); be lenient about extra bytes.
pub fn parse_midi(bytes: &[u8]) -> Option<MidiEvent> {
    let (status, data) = bytes.split_first()?;
    let kind = status & 0xF0;
    match kind {
        0x90 if data.len() >= 2 => {
            let (note, velocity) = (data[0], data[1].min(127));
            if velocity == 0 {
                Some(MidiEvent::NoteOff { note })
            } else {
                Some(MidiEvent::NoteOn { note, velocity })
            }
        }
        0x80 if data.len() >= 2 => Some(MidiEvent::NoteOff { note: data[0] }),
        _ => None,
    }
}

/// Map a MIDI note onto the active instrument:
///
/// - pitched instrument → its pitched target, note preserved;
/// - drum kit → chromatic keys from C1 (36) across targets in order;
/// - anything else → None (no silent misfires).
///
/// Velocity is passed through; 0 is handled as note-off before mapping.
pub fn map_note(instrument: &Instrument, note: u8, velocity: u8) -> Option<(TargetId, u8, u8)> {
    if velocity == 0 {
        return None;
    }
    if let Some(t) = instrument
        .targets
        .iter()
        .find(|t| matches!(t.selector, crate::instrument::Selector::Pitched { .. }))
    {
        return Some((t.id, note, velocity));
    }
    let index = note.checked_sub(36)? as usize;
    instrument.targets.get(index).map(|t| (t.id, 60, velocity))
}

/// Resolve the sounding target for a note-off (same mapping, no velocity).
pub fn map_release(instrument: &Instrument, note: u8) -> Option<(TargetId, u8)> {
    const DUMMY_VELOCITY: u8 = 64;
    map_note(instrument, note, DUMMY_VELOCITY).map(|(target, play_note, _)| (target, play_note))
}

/// Available MIDI input port names. Never panics, even with no devices.
pub fn list_inputs() -> Vec<String> {
    let Ok(input) = midir::MidiInput::new("zearo-scan") else {
        return Vec::new();
    };
    input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .collect()
}

#[derive(Debug)]
pub enum MidiError {
    Init(String),
    NotFound(String),
    Connect(String),
    Lost(String),
}

impl std::fmt::Display for MidiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(e) => write!(f, "MIDI init failed: {e}"),
            Self::NotFound(e) => write!(f, "MIDI device not found ({e})"),
            Self::Connect(e) => write!(f, "MIDI connect failed: {e}"),
            Self::Lost(e) => write!(f, "MIDI connection lost: {e}"),
        }
    }
}

impl std::error::Error for MidiError {}

/// Live connection: owns the midir connection (dropping disconnects) and
/// the receiving end of the byte queue.
pub struct MidiBridge {
    _connection: midir::MidiInputConnection<Sender<Vec<u8>>>,
    rx: Receiver<Vec<u8>>,
    port_name: String,
}

impl MidiBridge {
    /// Connect to the first port whose name contains `want`
    /// (case-insensitive). Returns the bridge; errors never panic.
    pub fn connect_matching(want: &str) -> Result<Self, MidiError> {
        let input = midir::MidiInput::new("zearo").map_err(|e| MidiError::Init(e.to_string()))?;
        let ports = input.ports();
        let needle = want.to_lowercase();
        let found = ports.iter().find_map(|port| {
            input
                .port_name(port)
                .ok()
                .filter(|name| name.to_lowercase().contains(&needle))
                .map(|name| (port, name))
        });
        let (port, name) = found.ok_or_else(|| MidiError::NotFound(want.to_string()))?;
        let (tx, rx) = mpsc::channel();
        let connection = input
            .connect(
                port,
                "zearo-in",
                move |_stamp, bytes: &[u8], tx: &mut Sender<Vec<u8>>| {
                    let _ = tx.send(bytes.to_vec());
                },
                tx,
            )
            .map_err(|e| MidiError::Connect(e.to_string()))?;
        Ok(Self {
            _connection: connection,
            rx,
            port_name: name,
        })
    }

    /// Prefer the KeyStep, else the first available input.
    pub fn connect_keystep_or_first() -> Result<Self, MidiError> {
        Self::connect_matching("keystep").or_else(|_| {
            let names = list_inputs();
            let first = names
                .into_iter()
                .next()
                .ok_or_else(|| MidiError::NotFound("no inputs".to_string()))?;
            Self::connect_matching(&first)
        })
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Drain parsed events. Disconnects surface as an empty drain — midir
    /// gives no hotplug signal on Windows, so the UI offers Rescan.
    pub fn poll(&mut self) -> Vec<MidiEvent> {
        let mut out = Vec::new();
        while let Ok(bytes) = self.rx.try_recv() {
            if let Some(event) = parse_midi(&bytes) {
                out.push(event);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{percussion_kit, pitched_pluck, snare9};
    use crate::sample::AssetPool;

    #[test]
    fn parses_note_on_off_across_channels() {
        assert_eq!(
            parse_midi(&[0x90, 60, 100]),
            Some(MidiEvent::NoteOn {
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(
            parse_midi(&[0x9F, 0, 1]),
            Some(MidiEvent::NoteOn {
                note: 0,
                velocity: 1
            })
        );
        // Velocity-0 note-on is note-off.
        assert_eq!(
            parse_midi(&[0x90, 60, 0]),
            Some(MidiEvent::NoteOff { note: 60 })
        );
        assert_eq!(
            parse_midi(&[0x80, 64, 40]),
            Some(MidiEvent::NoteOff { note: 64 })
        );
        assert_eq!(parse_midi(&[0xB0, 7, 100]), None, "CC ignored");
        assert_eq!(parse_midi(&[0xF8]), None, "clock ignored");
        assert_eq!(parse_midi(&[0x90, 60]), None, "truncated ignored");
        assert_eq!(parse_midi(&[]), None, "empty ignored");
        // Extra bytes tolerated (sysex-ish leftovers never crash us).
        assert_eq!(
            parse_midi(&[0x92, 60, 100, 0xF7]),
            Some(MidiEvent::NoteOn {
                note: 60,
                velocity: 100
            })
        );
    }

    #[test]
    fn kit_maps_chromatic_from_c1() {
        let pool = AssetPool::generate();
        let kit = percussion_kit(&pool);
        assert_eq!(map_note(&kit, 36, 100), Some((TargetId(0), 60, 100)));
        assert_eq!(map_note(&kit, 45, 90), Some((TargetId(9), 60, 90)));
        assert_eq!(map_note(&kit, 35, 100), None, "below C1: silent, not wrong");
        assert_eq!(map_note(&kit, 99, 100), None, "past the last voice: silent");
        assert_eq!(
            map_note(&kit, 40, 0),
            None,
            "vel 0 never maps (note-off path)"
        );
        assert_eq!(map_release(&kit, 36), Some((TargetId(0), 60)));
    }

    #[test]
    fn pitched_preserves_notes() {
        let pool = AssetPool::generate();
        let pluck = pitched_pluck(&pool);
        assert_eq!(map_note(&pluck, 48, 100), Some((TargetId(0), 48, 100)));
        assert_eq!(map_note(&pluck, 72, 100), Some((TargetId(0), 72, 100)));
        assert_eq!(map_note(&pluck, 0, 100), Some((TargetId(0), 0, 100)));
        // Single-target snare maps its one voice from C1.
        let snare = snare9(&pool);
        assert_eq!(map_note(&snare, 36, 100), Some((TargetId(0), 60, 100)));
        assert_eq!(map_note(&snare, 37, 100), None);
    }

    #[test]
    fn listing_never_panics_and_missing_device_errors_cleanly() {
        let _ = list_inputs();
        assert!(matches!(
            MidiBridge::connect_matching("definitely-not-a-midi-device-xyz"),
            Err(MidiError::NotFound(_))
        ));
    }
}
