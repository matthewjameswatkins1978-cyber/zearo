//! `zearo` workbench probe binary.
//!
//! Exercises the verified stack (DSP, ring buffer, audio/MIDI enumeration)
//! and, with `--ui`, opens the minimal `egui` workbench window.

use zearo::{
    input_device_names, midi_input_names, output_device_names, render_sine, rms, rtrb_roundtrip,
};

fn main() {
    let with_ui = std::env::args().any(|arg| arg == "--ui");

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

    println!("rtrb: roundtrip(0.25) = {}", rtrb_roundtrip(0.25));

    print_list("audio outputs", &output_device_names());
    print_list("audio inputs", &input_device_names());
    print_list("midi inputs", &midi_input_names());

    if with_ui {
        if let Err(err) = run_ui() {
            eprintln!("ui error: {err}");
            std::process::exit(1);
        }
    } else {
        println!("ui: skipped (pass --ui to open the workbench window)");
    }
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

#[derive(Default)]
struct ZearoApp {
    level: f32,
}

impl eframe::App for ZearoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Zearo workbench");
            ui.label("Rust-first groovebox stack is live.");
            ui.add(egui::Slider::new(&mut self.level, 0.0..=1.0).text("monitor level"));
        });
    }
}

fn run_ui() -> eframe::Result<()> {
    eframe::run_native(
        "Zearo workbench",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(ZearoApp::default()) as Box<dyn eframe::App>)),
    )
}
