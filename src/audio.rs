//! Audio hardware backend (cpal).
//!
//! Two phases so the app never guesses the device rate: [`probe_output`]
//! reports the preferred device *without* opening a stream, the app builds
//! its engine/sequencer, then [`start`] opens the stream and retunes both to
//! the real rate. Device preference is a pure function ([`rank_output`]) so
//! the iD22-first policy is unit-tested without hardware.
//!
//! WASAPI is the supported Windows path for this milestone. ASIO needs the
//! Steinberg SDK + clang and stays unclaimed until proven on the real iD22.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::Consumer;

use crate::engine::{Engine, Msg};
use crate::pattern::{LiveViews, Sequencer};

#[derive(Debug)]
pub enum AudioError {
    NoOutputs,
    Device(String),
    Config(String),
    Stream(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOutputs => write!(f, "no audio output devices found"),
            Self::Device(e) => write!(f, "audio device error: {e}"),
            Self::Config(e) => write!(f, "audio config error: {e}"),
            Self::Stream(e) => write!(f, "audio stream error: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub host: String,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Prefer the Audient iD22; otherwise the system default (first listed).
/// Pure: the policy is tested without touching hardware.
pub fn rank_output(names: &[String]) -> Option<usize> {
    names
        .iter()
        .position(|n| n.to_lowercase().contains("id22"))
        .or(if names.is_empty() { None } else { Some(0) })
}

fn output_names(host: &cpal::Host) -> Result<Vec<(cpal::Device, String)>, AudioError> {
    let devices = host
        .output_devices()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    let mut out = Vec::new();
    for device in devices {
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "<unnamed>".to_string());
        out.push((device, name));
    }
    Ok(out)
}

/// Report the preferred output without opening anything.
pub fn probe_output() -> Result<OutputInfo, AudioError> {
    let host = cpal::default_host();
    let listed = output_names(&host)?;
    if listed.is_empty() {
        return Err(AudioError::NoOutputs);
    }
    let names: Vec<String> = listed.iter().map(|(_, n)| n.clone()).collect();
    let index = rank_output(&names).ok_or(AudioError::NoOutputs)?;
    let (device, name) = &listed[index];
    let config = device
        .default_output_config()
        .map_err(|e| AudioError::Config(e.to_string()))?;
    Ok(OutputInfo {
        host: format!("{:?}", host.id()),
        name: name.clone(),
        sample_rate: config.sample_rate(),
        channels: config.channels(),
    })
}

/// Opened stream. Drop to stop; keep alive while playing.
pub struct RunningAudio {
    _stream: cpal::Stream,
    pub info: OutputInfo,
}

/// Open the preferred output, retune the engine + transport to the real
/// device rate, and start the callback. The callback owns the engine and
/// sequencer; the UI keeps the message producer and live views.
pub fn start(
    mut engine: Engine,
    mut seq: Sequencer,
    mut msgs: Consumer<Msg>,
    views: Arc<LiveViews>,
) -> Result<RunningAudio, AudioError> {
    let host = cpal::default_host();
    let host_name = format!("{:?}", host.id());
    let mut listed = output_names(&host)?;
    if listed.is_empty() {
        return Err(AudioError::NoOutputs);
    }
    let names: Vec<String> = listed.iter().map(|(_, n)| n.clone()).collect();
    let index = rank_output(&names).ok_or(AudioError::NoOutputs)?;
    let (device, name) = listed.drain(index..=index).next().unwrap();
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Config(e.to_string()))?;
    let sample_rate = supported.sample_rate();
    let channels = supported.channels();
    let config: cpal::StreamConfig = supported.into();

    engine.set_device_rate(sample_rate as f32);
    seq.transport.sample_rate = f64::from(sample_rate);

    // Reused scratch buffers: grown rarely, never per-frame allocated in
    // steady state.
    let mut scratch_l = Vec::<f32>::new();
    let mut scratch_r = Vec::<f32>::new();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _| {
                let frames = data.len() / channels.max(1) as usize;
                if scratch_l.len() < frames {
                    scratch_l.resize(frames, 0.0);
                    scratch_r.resize(frames, 0.0);
                }
                seq.process_block(
                    &mut engine,
                    &mut msgs,
                    &mut scratch_l[..frames],
                    &mut scratch_r[..frames],
                );
                seq.publish(&views);
                match channels {
                    1 => {
                        for (i, frame) in data.iter_mut().enumerate() {
                            *frame = scratch_l[i];
                        }
                    }
                    _ => {
                        for (i, frame) in data.chunks_mut(channels as usize).enumerate() {
                            frame[0] = scratch_l[i];
                            if frame.len() > 1 {
                                frame[1] = scratch_r[i];
                            }
                        }
                    }
                }
            },
            |err| eprintln!("zearo audio error: {err}"),
            None,
        )
        .map_err(|e| AudioError::Stream(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::Stream(e.to_string()))?;
    Ok(RunningAudio {
        _stream: stream,
        info: OutputInfo {
            host: host_name,
            name,
            sample_rate,
            channels,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id22_wins_over_default() {
        let names = vec![
            "Realtek Digital Output (Realtek(R) Audio)".to_string(),
            "Output 1/2 (Audient iD22)".to_string(),
            "1 - VG27A (AMD High Definition Audio Device)".to_string(),
        ];
        assert_eq!(rank_output(&names), Some(1));
    }

    #[test]
    fn default_wins_without_id22_and_empty_is_none() {
        let names = vec!["Speakers".to_string(), "Headphones".to_string()];
        assert_eq!(rank_output(&names), Some(0));
        let empty: Vec<String> = Vec::new();
        assert_eq!(rank_output(&empty), None);
    }

    #[test]
    fn probe_does_not_panic_and_reports_something_usable() {
        // Hardware-dependent: passes with or without devices, but never
        // panics. On the dev machine it must find the real outputs.
        match probe_output() {
            Ok(info) => {
                assert!(info.sample_rate >= 8000, "sane rate {}", info.sample_rate);
                assert!(!info.name.is_empty());
            }
            Err(AudioError::NoOutputs) => {}
            Err(e) => panic!("unexpected probe failure: {e}"),
        }
    }
}
