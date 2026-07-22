//! Byte-exact round-trip tests: for every well-formed fixture, `parse`
//! followed by `serialize` must reproduce the original file exactly.

use std::fs;
use std::path::PathBuf;

use scenegraph_core::Document;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Parse `name` (strict mode) and assert the serialized output is
/// byte-for-byte identical to the file on disk.
fn assert_roundtrip(name: &str) {
    let path = fixtures_dir().join(name);
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let source = String::from_utf8(bytes.clone()).unwrap_or_else(|e| panic!("{} is not UTF-8: {e}", path.display()));

    let doc = Document::parse(&source).unwrap_or_else(|e| panic!("{} failed to parse: {e}", path.display()));
    let output = doc.serialize();

    assert_eq!(
        output.as_bytes(),
        bytes.as_slice(),
        "{} did not round-trip byte-for-byte",
        path.display()
    );
}

#[test]
fn basic_2d_scene() {
    assert_roundtrip("01_basic_2d_scene.tscn");
}

#[test]
fn animation_player() {
    assert_roundtrip("02_animation_player.tscn");
}

#[test]
fn shader_multiline_string() {
    assert_roundtrip("03_shader_multiline.tres");
}

#[test]
fn packed_arrays() {
    assert_roundtrip("04_packed_arrays.tscn");
}

#[test]
fn scene_inheritance() {
    assert_roundtrip("05_scene_inheritance.tscn");
}

#[test]
fn crlf_line_endings() {
    assert_roundtrip("06_crlf.tscn");
}

#[test]
fn groups_and_nested_dict() {
    assert_roundtrip("07_groups_and_dict.tscn");
}

#[test]
fn minimal_scene() {
    assert_roundtrip("08_minimal.tscn");
}

#[test]
fn no_trailing_newline() {
    assert_roundtrip("09_no_trailing_newline.tscn");
}

#[test]
fn utf8_bom() {
    assert_roundtrip("10_bom.tscn");
}

#[test]
fn mixed_line_endings() {
    assert_roundtrip("11_mixed_line_endings.tscn");
}

/// Safety net: every `.tscn`/`.tres` directly under `fixtures/` (not the
/// `invalid/` subdirectory) round-trips, even ones not individually named
/// above.
#[test]
fn every_fixture_round_trips() {
    let dir = fixtures_dir();
    let mut checked = 0usize;
    for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display())) {
        let entry = entry.unwrap();
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if !matches!(ext, Some("tscn") | Some("tres")) {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        assert_roundtrip(&name);
        checked += 1;
    }
    assert!(checked >= 8, "expected at least 8 fixtures, found {checked}");
}

#[test]
fn empty_document_round_trips() {
    let doc = Document::parse("").unwrap();
    assert_eq!(doc.serialize(), "");
}
