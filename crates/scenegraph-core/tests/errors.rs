//! Tests that malformed input is rejected (strict mode) or recovered from
//! (tolerant mode) without ever panicking.

use std::fs;
use std::path::PathBuf;

use scenegraph_core::Document;

fn read_invalid_fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/invalid")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[test]
fn broken_header_is_rejected_in_strict_mode() {
    let src = read_invalid_fixture("broken_header.tscn");
    let err = Document::parse(&src).expect_err("expected a parse error for an unterminated header");
    assert!(err.line >= 1);
    assert!(!err.message.is_empty());
}

#[test]
fn broken_header_recovers_in_tolerant_mode_and_still_round_trips() {
    let src = read_invalid_fixture("broken_header.tscn");
    let (doc, diagnostics) = Document::parse_tolerant(&src);
    assert!(!diagnostics.is_empty());
    assert_eq!(doc.serialize(), src, "tolerant mode must still preserve every byte");
}

#[test]
fn unterminated_string_is_rejected_in_strict_mode() {
    let src = read_invalid_fixture("unterminated_string.tres");
    let err = Document::parse(&src).expect_err("expected a parse error for an unclosed string");
    assert!(!err.message.is_empty());
}

#[test]
fn unterminated_string_recovers_in_tolerant_mode_and_still_round_trips() {
    let src = read_invalid_fixture("unterminated_string.tres");
    let (doc, diagnostics) = Document::parse_tolerant(&src);
    assert!(!diagnostics.is_empty());
    assert_eq!(doc.serialize(), src);
}

#[test]
fn malformed_section_is_rejected_in_strict_mode() {
    let src = read_invalid_fixture("bad_section.tscn");
    let err = Document::parse(&src).expect_err("expected a parse error for a malformed property value");
    assert!(!err.message.is_empty());
}

#[test]
fn malformed_section_recovers_in_tolerant_mode_and_still_round_trips() {
    let src = read_invalid_fixture("bad_section.tscn");
    let (doc, diagnostics) = Document::parse_tolerant(&src);
    assert!(!diagnostics.is_empty());
    assert_eq!(doc.serialize(), src);
}

#[test]
fn empty_input_parses_to_an_empty_document() {
    let doc = Document::parse("").expect("empty input is well-formed");
    assert_eq!(doc.section_count(), 0);
    assert_eq!(doc.serialize(), "");
    assert!(doc.file_descriptor().is_none());
    assert!(doc.build_tree().is_err());
}

#[test]
fn only_whitespace_parses_cleanly() {
    let src = "\n\n   \n\t\n";
    let doc = Document::parse(src).expect("whitespace-only input is well-formed");
    assert_eq!(doc.serialize(), src);
}

#[test]
fn stray_closing_bracket_does_not_panic() {
    // Not a valid header (no matching opening bracket at this position),
    // but must not panic; it is preserved as an unrecognized line.
    let src = "]not a header\n[gd_scene format=3]\n";
    let (doc, _diags) = Document::parse_tolerant(src);
    assert_eq!(doc.serialize(), src);
}

#[test]
fn deeply_nested_unbalanced_brackets_do_not_panic() {
    let src = "[gd_scene format=3]\n\n[node name=\"A\" type=\"Node\"]\nv = [[[[[[[[[[1\n";
    let (doc, diags) = Document::parse_tolerant(src);
    assert!(!diags.is_empty());
    assert_eq!(doc.serialize(), src);
}
