//! Physical audio input check — ignored by default.
//!
//! Run with the iD22 connected:
//! `cargo test --locked -- --ignored input_capture_flows`
//! Verifies the recording path opens the real device and frames flow.

#[test]
#[ignore]
fn input_capture_flows() {
    let cap = zearo::audio::start_input_capture().expect("input opens");
    assert!(
        cap.info.name.to_lowercase().contains("id22"),
        "uses the iD22: {}",
        cap.info.name
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut frames = 0usize;
    while let Ok(chunk) = cap.rx.try_recv() {
        frames += chunk.len();
    }
    assert!(frames > 1000, "frames flow from the device (got {frames})");
    drop(cap);
}
