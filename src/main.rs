//! `zearo` binary: desktop app, headless probe, headless demo playback.
//!
//! - `zearo` → full app (8×8 grid, layouts, transport, recording)
//! - `zearo --probe` → device enumeration only, no stream opened
//! - `zearo --demo [bars]` → headless loop playback on the preferred output
//!   with a stats report (automated audio-path proof, no display needed)
//! - `zearo --input-test [secs]` → capture from the preferred input and
//!   report what arrived (proves the recording path on real hardware)

use std::sync::Arc;

use zearo::{input_device_names, midi_input_names, output_device_names, render_sine, rms};

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--probe") => probe(),
        Some("--demo") => demo(),
        Some("--input-test") => input_test(),
        Some("--help" | "-h") => help(),
        _ => zearo::app::run(),
    }
}

fn help() {
    println!("zearo — playable groovebox prototype");
    println!("  zearo            desktop app (grid + layouts + transport)");
    println!("  zearo --probe    list audio/MIDI devices, no stream");
    println!("  zearo --demo [N] play the demo loop N bars headless (default 4)");
    println!("  zearo --input-test [S] capture S seconds of input (default 2)");
}

fn probe() {
    let wave = render_sine(1.0, 44100.0);
    println!(
        "dsp: rendered {:.3} s @ {:.0} Hz, {} channel(s), {} frames",
        wave.duration(),
        wave.sample_rate(),
        wave.channels(),
        wave.len()
    );
    println!(
        "dsp: channel 0 rms = {:.6} (theory 0.707107)",
        rms(wave.channel(0))
    );
    print_list("audio outputs", &output_device_names());
    print_list("audio inputs", &input_device_names());
    print_list("midi inputs", &midi_input_names());
}

fn print_list(kind: &str, names: &[String]) {
    if names.is_empty() {
        println!("{kind}: none visible (headless-safe, no hardware required)");
    } else {
        println!("{kind} ({}):", names.len());
        for name in names {
            println!("  - {name}");
        }
    }
}

fn demo() {
    let bars: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4)
        .max(1);
    let (song, pool) = zearo::app::demo_project();
    let (live, mut handles) = zearo::app::spawn_all(&song, &pool);
    let views: Vec<Arc<zearo::pattern::LiveViews>> =
        handles.iter().map(|h| Arc::clone(&h.views)).collect();
    match zearo::audio::start_multi(live, views) {
        Ok(running) => {
            println!(
                "zearo demo: {} @ {} Hz, {} ch",
                running.info.name, running.info.sample_rate, running.info.channels
            );
            for h in &mut handles {
                let _ = h.msg_tx.push(zearo::engine::Msg::Play);
            }
            let bar_secs = 60.0 / song.bpm * 4.0;
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs_f64(bar_secs * f64::from(bars) + 2.0);
            let mut retired = 0u64;
            let mut peak = 0.0f32;
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(200));
                for h in &mut handles {
                    retired += zearo::engine::drain_retired(&mut h.retire_rx) as u64;
                    peak = peak.max(h.views.take_peak());
                }
            }
            println!(
                "zearo demo: played {bars} bars + tail; retired {retired} buffers; peak {peak:.3}; stream errors: {}",
                running.error_count()
            );
        }
        Err(e) => {
            eprintln!("zearo demo: audio failed: {e}");
            std::process::exit(1);
        }
    }
}

fn input_test() {
    let secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2)
        .clamp(1, 10);
    match zearo::audio::start_input_capture() {
        Ok(cap) => {
            println!(
                "zearo input: {} @ {} Hz, {} ch — capturing {secs}s",
                cap.info.name, cap.info.sample_rate, cap.info.channels
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            let mut frames = 0usize;
            let mut peak = 0.0f32;
            let mut sum = 0.0f64;
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(100));
                while let Ok(chunk) = cap.rx.try_recv() {
                    frames += chunk.len();
                    for s in &chunk {
                        peak = peak.max(s.abs());
                        sum += f64::from(*s) * f64::from(*s);
                    }
                }
            }
            let rms = if frames > 0 {
                (sum / frames as f64).sqrt()
            } else {
                0.0
            };
            println!("zearo input: {frames} frames, peak {peak:.3}, rms {rms:.4}");
        }
        Err(e) => {
            eprintln!("zearo input failed: {e}");
            std::process::exit(1);
        }
    }
}
