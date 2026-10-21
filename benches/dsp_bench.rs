//! Offline DSP benchmark: render 1 s of 440 Hz sine at 44.1 kHz.
use criterion::{criterion_group, criterion_main, Criterion};
use zearo::render_sine;

fn bench_render_sine(c: &mut Criterion) {
    c.bench_function("render_sine_1s_44100", |b| {
        b.iter(|| render_sine(1.0, 44100.0))
    });
}

criterion_group!(benches, bench_render_sine);
criterion_main!(benches);
