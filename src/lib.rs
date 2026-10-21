//! Zearo groovebox workbench library.
//!
//! Minimal interop surface for the verified development stack:
//! offline DSP rendering (`fundsp`), realtime-safe messaging (`rtrb`),
//! and audio/MIDI device enumeration (`cpal`, `midir`).

use cpal::traits::{DeviceTrait, HostTrait};
pub mod instrument;
pub mod sample;

use fundsp::prelude::*;

/// Render `seconds` of a 440 Hz sine tone at `sample_rate` Hz.
///
/// Offline only: needs no audio hardware, so it runs in CI and tests.
pub fn render_sine(seconds: f64, sample_rate: f64) -> Wave {
    Wave::render(sample_rate, seconds, &mut sine_hz::<f32>(440.0))
}

/// Root-mean-square level of mono `samples`.
///
/// Returns 0.0 for an empty slice.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// Round-trip one sample through a realtime-safe SPSC ring buffer.
pub fn rtrb_roundtrip(value: f32) -> f32 {
    let (mut producer, mut consumer) = rtrb::RingBuffer::new(8);
    producer.push(value).expect("ring has room for one value");
    consumer.pop().expect("ring holds the pushed value")
}

/// Names of available audio output devices (informational; may be empty).
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|devices| {
            devices
                .filter_map(|d| d.description().ok().map(|desc| desc.name().to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// Names of available audio input devices (informational; may be empty).
pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devices| {
            devices
                .filter_map(|d| d.description().ok().map(|desc| desc.name().to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// Names of available MIDI input ports (informational; may be empty).
pub fn midi_input_names() -> Vec<String> {
    let input = match midir::MidiInput::new("zearo-probe") {
        Ok(input) => input,
        Err(_) => return Vec::new(),
    };
    input
        .ports()
        .iter()
        .filter_map(|port| input.port_name(port).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_rms_matches_theory() {
        let wave = render_sine(1.0, 44100.0);
        assert_eq!(wave.channels(), 1);
        assert_eq!(wave.len(), 44100);
        let level = rms(wave.channel(0));
        let theory = core::f32::consts::FRAC_1_SQRT_2;
        assert!((level - theory).abs() < 1e-4, "rms={level}");
    }

    #[test]
    fn ring_buffer_roundtrip() {
        assert_eq!(rtrb_roundtrip(0.5), 0.5);
    }

    #[test]
    fn enumeration_does_not_panic() {
        let _ = output_device_names();
        let _ = input_device_names();
        let _ = midi_input_names();
    }
}
