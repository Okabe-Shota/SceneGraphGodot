//! Black-box tests for the `sg` binary against the fixture corpus.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn sg() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sg"))
}

#[test]
fn roundtrip_succeeds_for_every_well_formed_fixture() {
    let dir = fixtures_dir();
    let mut checked = 0usize;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if !matches!(ext, Some("tscn") | Some("tres")) {
            continue;
        }
        let output = sg().arg("roundtrip").arg(&path).output().expect("failed to run sg");
        assert!(
            output.status.success(),
            "sg roundtrip failed for {}\nstdout: {}\nstderr: {}",
            path.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        checked += 1;
    }
    assert!(checked >= 8, "expected at least 8 fixtures, found {checked}");
}

#[test]
fn roundtrip_fails_on_broken_header() {
    let path = fixtures_dir().join("invalid/broken_header.tscn");
    let output = sg().arg("roundtrip").arg(&path).output().expect("failed to run sg");
    assert!(!output.status.success());
}

#[test]
fn parse_prints_stats_for_basic_scene() {
    let path = fixtures_dir().join("01_basic_2d_scene.tscn");
    let output = sg().arg("parse").arg(&path).output().expect("failed to run sg");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sections:"));
    assert!(stdout.contains("nodes:"));
    assert!(stdout.contains("references:"));
}

#[test]
fn parse_does_not_panic_on_broken_input() {
    let path = fixtures_dir().join("invalid/unterminated_string.tres");
    let output = sg().arg("parse").arg(&path).output().expect("failed to run sg");
    // Tolerant mode: the process must exit cleanly (no panic / abort
    // signal), even though it reports the recovered diagnostics as a
    // failure exit code.
    assert!(
        output.status.code().is_some(),
        "process did not exit normally (possible panic/abort)"
    );
}

#[test]
fn missing_file_reports_error_without_panicking() {
    let path = fixtures_dir().join("does_not_exist.tscn");
    let output = sg().arg("parse").arg(&path).output().expect("failed to run sg");
    assert!(!output.status.success());
    assert!(output.status.code().is_some());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error"));
}
