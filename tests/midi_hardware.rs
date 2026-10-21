//! Physical MIDI hardware checks — ignored by default.
//!
//! Run explicitly with the KeyStep plugged in:
//! `cargo test --locked -- --ignored midi_hardware`
//! These prove the real device path (enumerate + open + clean drop).
//! Note flow itself is covered by `midi::tests` with captured byte streams.

use zearo::midi::{MidiBridge, list_inputs};

#[test]
#[ignore]
fn keystep_is_present_and_connectable() {
    let names = list_inputs();
    assert!(
        !names.is_empty(),
        "expected at least one MIDI input on the dev machine"
    );
    let keystep: Vec<_> = names.iter().filter(|n| n.to_lowercase().contains("keystep")).collect();
    assert!(!keystep.is_empty(), "KeyStep not found among {names:?}");
    let bridge = MidiBridge::connect_matching("keystep")
        .expect("KeyStep connects cleanly");
    assert!(bridge.port_name().to_lowercase().contains("keystep"));
    // Dropping disconnects; a second connect must also work (no stuck handle).
    drop(bridge);
    let _reconnected =
        MidiBridge::connect_matching("keystep").expect("reconnect works after drop");
}
