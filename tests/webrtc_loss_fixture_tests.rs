//! Exercise the local UDP fault fixture with real sockets and verdict controls.

use std::process::Command;

#[test]
#[cfg_attr(miri, ignore)]
fn udp_loss_fixture_preserves_packets_and_requires_observed_faults() {
    let output = Command::new("python3")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "clients/native/tests/fixtures",
            "-p",
            "test_webrtc_loss_control.py",
        ])
        .output()
        .expect("run UDP loss fixture controls");
    assert!(
        output.status.success(),
        "UDP loss fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
