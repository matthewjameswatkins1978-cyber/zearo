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

use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::Consumer;

use crate::engine::{Engine, Msg, Shared};
use crate::pattern::{LiveViews, Sequencer};

#[derive(Debug)]
pub enum AudioError {
    NoOutputs,
    NoInputs,
    Device(String),
    Config(String),
    Stream(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOutputs => write!(f, "no audio output devices found"),
            Self::NoInputs => write!(f, "no audio input devices found"),
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
/// Prefer the Audient iD22; among its outputs prefer the main monitor pair
/// ("1/2"); otherwise the system default (first listed). Pure: the policy
/// is unit-tested without touching hardware.
pub fn rank_output(names: &[String]) -> Option<usize> {
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    lower
        .iter()
        .position(|n| n.contains("id22") && n.contains("1/2"))
        .or_else(|| lower.iter().position(|n| n.contains("id22")))
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
    /// Stream errors observed in callbacks (xruns, device faults).
    pub errors: Arc<AtomicU64>,
}

impl RunningAudio {
    pub fn error_count(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
}

/// One sounding track handed to the audio thread.
pub struct LiveTrack {
    pub engine: Engine,
    pub seq: Sequencer,
    pub msgs: Consumer<Msg>,
    pub shared: Arc<Shared>,
}

/// Open the preferred output for a single track. See [`start_multi`].
pub fn start(
    engine: Engine,
    seq: Sequencer,
    msgs: Consumer<Msg>,
    views: Arc<LiveViews>,
    shared: Arc<Shared>,
) -> Result<RunningAudio, AudioError> {
    start_multi(
        vec![LiveTrack {
            engine,
            seq,
            msgs,
            shared,
        }],
        vec![views],
    )
}

/// Open the preferred output, retune every engine + transport to the real
/// device rate, and start the callback. Tracks mix additively; the callback
/// owns them while the UI keeps producers, shared handles and live views.
pub fn start_multi(
    mut tracks: Vec<LiveTrack>,
    views: Vec<Arc<LiveViews>>,
) -> Result<RunningAudio, AudioError> {
    assert_eq!(tracks.len(), views.len(), "one view set per track");
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

    for track in &mut tracks {
        track.engine.set_device_rate(sample_rate as f32);
        track.seq.transport.sample_rate = f64::from(sample_rate);
    }

    let errors = Arc::new(AtomicU64::new(0));
    let err_count = Arc::clone(&errors);

    // Reused scratch buffers: grown rarely, never per-frame allocated in
    // steady state. Master accumulates the track mix.
    let mut scratch_l = Vec::<f32>::new();
    let mut scratch_r = Vec::<f32>::new();
    let mut master_l = Vec::<f32>::new();
    let mut master_r = Vec::<f32>::new();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _| {
                let frames = data.len() / channels.max(1) as usize;
                for buf in [&mut scratch_l, &mut scratch_r, &mut master_l, &mut master_r] {
                    if buf.len() < frames {
                        buf.resize(frames, 0.0);
                    }
                }
                for m in master_l[..frames]
                    .iter_mut()
                    .chain(master_r[..frames].iter_mut())
                {
                    *m = 0.0;
                }
                for (ti, track) in tracks.iter_mut().enumerate() {
                    track.engine.sync(&track.shared);
                    track.seq.process_block(
                        &mut track.engine,
                        &mut track.msgs,
                        &mut scratch_l[..frames],
                        &mut scratch_r[..frames],
                    );
                    track.seq.publish(&views[ti]);
                    views[ti]
                        .dropped
                        .store(track.engine.retired_dropped, Ordering::Relaxed);
                    for i in 0..frames {
                        master_l[i] += scratch_l[i];
                        master_r[i] += scratch_r[i];
                    }
                }
                let mut peak = 0.0f32;
                for i in 0..frames {
                    let a = master_l[i].abs();
                    let b = master_r[i].abs();
                    if a > peak {
                        peak = a;
                    }
                    if b > peak {
                        peak = b;
                    }
                }
                if !views.is_empty() {
                    let prev = f32::from_bits(views[0].peak_bits.load(Ordering::Relaxed));
                    if peak > prev {
                        views[0].peak_bits.store(peak.to_bits(), Ordering::Relaxed);
                    }
                }
                match channels {
                    1 => {
                        for (i, frame) in data.iter_mut().enumerate() {
                            *frame = master_l[i];
                        }
                    }
                    _ => {
                        for (i, frame) in data.chunks_mut(channels as usize).enumerate() {
                            frame[0] = master_l[i];
                            if frame.len() > 1 {
                                frame[1] = master_r[i];
                            }
                        }
                    }
                }
            },
            move |err| {
                eprintln!("zearo audio error: {err}");
                err_count.fetch_add(1, Ordering::Relaxed);
            },
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
        errors,
    })
}

/// Prefer the iD22 input (main pair 1/2 first); otherwise the system default.
/// Pure: unit-tested without hardware.
pub fn rank_input(names: &[String]) -> Option<usize> {
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    lower
        .iter()
        .position(|n| n.contains("id22") && n.contains("1/2"))
        .or_else(|| lower.iter().position(|n| n.contains("id22")))
        .or(if names.is_empty() { None } else { Some(0) })
}

#[derive(Debug, Clone)]
pub struct InputInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Open the preferred input and forward mono f32 chunks to the UI thread.
/// Capture plumbing only — accumulation, analysis and preparation all run
/// off-thread, never on an audio callback.
pub struct InputCapture {
    _stream: cpal::Stream,
    pub info: InputInfo,
    pub rx: mpsc::Receiver<Vec<f32>>,
}

pub fn start_input_capture() -> Result<InputCapture, AudioError> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    let mut listed: Vec<(cpal::Device, String)> = Vec::new();
    for device in devices {
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "<unnamed>".to_string());
        listed.push((device, name));
    }
    if listed.is_empty() {
        return Err(AudioError::NoInputs);
    }
    let names: Vec<String> = listed.iter().map(|(_, n)| n.clone()).collect();
    let index = rank_input(&names).ok_or(AudioError::NoInputs)?;
    let (device, name) = listed.drain(index..=index).next().unwrap();
    // Float path only: every WASAPI USB input we care about offers it,
    // and anything else fails loudly instead of mis-converting.
    let supported: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_input_configs()
        .map_err(|e| AudioError::Config(e.to_string()))?
        .collect();
    let best = supported
        .iter()
        .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
        .max_by_key(|c| c.max_sample_rate());
    let range = best.ok_or_else(|| AudioError::Config("no f32 input config".to_string()))?;
    let config: cpal::StreamConfig = range.with_max_sample_rate().into();
    let sample_rate = config.sample_rate;
    let channels = config.channels;
    let (tx, rx) = mpsc::channel();
    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _| {
                let frames = data.len() / channels.max(1) as usize;
                let mut mono = Vec::with_capacity(frames);
                for i in 0..frames {
                    let mut acc = 0.0f32;
                    for c in 0..channels as usize {
                        acc += data[i * channels as usize + c];
                    }
                    mono.push(acc / channels.max(1) as f32);
                }
                let _ = tx.send(mono);
            },
            |err| eprintln!("zearo input error: {err}"),
            None,
        )
        .map_err(|e| AudioError::Stream(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::Stream(e.to_string()))?;
    Ok(InputCapture {
        _stream: stream,
        info: InputInfo {
            name,
            sample_rate,
            channels,
        },
        rx,
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
    fn prefers_main_monitor_pair() {
        let names = vec![
            "Output 9/10 (Audient iD22)".to_string(),
            "Output 1/2 (Audient iD22)".to_string(),
            "Speakers".to_string(),
        ];
        assert_eq!(rank_output(&names), Some(1));
    }

    #[test]
    fn prefers_id22_input() {
        let names = vec![
            "Input 5/6 (Audient iD22)".to_string(),
            "Input 1/2 (Audient iD22)".to_string(),
        ];
        assert_eq!(rank_input(&names), Some(1));
        assert_eq!(rank_input(&[]), None);
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
