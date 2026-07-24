//! `sg i18n budget`: statically predicts UI text overflow across scene
//! files, before anything is sent for translation, without launching the
//! engine.
//!
//! # Design philosophy
//!
//! This is deliberately a linter, not an oracle. The owner's directive for
//! this feature: catching overflow before translation is worth more than
//! being 99% precise, because a check that always runs and prevents most
//! incidents beats a check that is never run at all. Concretely, that
//! means:
//!
//! - Font metrics are **approximate by design**. [`estimate_text_width`]
//!   uses a small static per-character width table (CJK/full-width
//!   characters count as ~1.0 em; Latin/proportional characters use a
//!   hand-picked em-relative table) rather than reading an actual font
//!   file. This is intentional, not a shortcut waiting to be fixed - v1
//!   never needs font-file-accurate metrics to be useful.
//! - Where a control's available width is **not** statically determinable
//!   (it stretches to fill a parent/container), the control is **skipped
//!   entirely** rather than guessed at - see [`available_width`]. A false
//!   alarm on every stretchy label would train users to ignore the tool;
//!   silence is the correct answer when the tool genuinely does not know.
//!   This is exactly where fixed-size buttons/labels are *not* skipped -
//!   and fixed-size controls are where overflow actually bites in
//!   practice.
//!
//! # Available-width precedence
//!
//! For each node, in order:
//!
//! 1. `custom_minimum_size = Vector2(W, H)` with `W > 0` -> available width
//!    is `W`. A Button/Label will not shrink below this, and in a fixed
//!    (non-stretching) layout cannot grow past it either, so text
//!    exceeding `W` is the canonical overflow case this tool targets.
//! 2. Otherwise, fixed offsets with non-stretching anchors: resolve
//!    `anchor_left`/`anchor_right` (defaulting an absent one to Godot's
//!    own default of `0.0` - most hand-authored/editor-exported scenes
//!    only write an anchor when it differs from that default), or fall
//!    back to `anchors_preset` when neither is present. If the anchors do
//!    not stretch horizontally (`anchor_left == anchor_right`, or an
//!    `anchors_preset` that is not one of the four horizontally-stretching
//!    presets - see [`HORIZONTAL_STRETCH_PRESETS`]) and both `offset_left`
//!    and `offset_right` are present, available width is
//!    `|offset_right - offset_left|`.
//! 3. Otherwise: **undeterminable**. The node is skipped, not warned
//!    about.
//!
//! `autowrap_mode` set to anything other than off (`0`) means the text
//! wraps vertically instead of overflowing horizontally - such a node is
//! skipped regardless of the above (see [`is_autowrapping`]).
//!
//! # Which strings are checked
//!
//! Only `text` and `placeholder_text` (see [`BUDGET_PROPERTIES`]) - the
//! single-line, fixed-width-prone properties. `tooltip_text` is never a
//! candidate at all (Godot's tooltip popup always sizes to fit its
//! content, so a width budget is meaningless for it - unlike the
//! "checked but skipped" cases above, this property is simply not in
//! [`BUDGET_PROPERTIES`]). `title`/`dialog_text` (`Window`/`AcceptDialog`)
//! are skipped in v1 for the same reason `sg i18n extract`'s module doc
//! comment gives for out-of-scope properties: window/dialog sizing is
//! managed by the windowing system, not a fixed control rect the way a
//! `Button`/`Label` inside a layout is - a future version could add a
//! dedicated (looser) budget for them, but that is a distinct problem
//! from "does this control's rect fit this text".

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use scenegraph_core::{parse_complete, Document, SectionInfo, Value};

use crate::i18n::ScanError;
use crate::nodegraph::{attr_str, build_node_graph, node_full_path, property_raw};
use crate::paths::collect_target_files;

/// Default `--expansion` value: assume translated text is, on average, 40%
/// wider (in estimated source-width terms) than the English source text.
/// This is a common rule of thumb for English-source UI localization
/// (German/Finnish/etc. commonly run 30-50% longer); it is a starting
/// point to tune per project, not a measured constant.
pub const DEFAULT_EXPANSION_PERCENT: u32 = 40;

/// Default `--default-font-size` value: Godot's own default `Control`
/// theme font size (`ThemeDB.fallback_font_size`) is 16px.
pub const DEFAULT_FONT_SIZE_PX: u32 = 16;

/// The issue code emitted for every overflow warning.
pub const CODE: &str = "i18n-text-overflow";

/// Node property names checked for overflow risk. See the module doc
/// comment ("Which strings are checked") for why this is a strict subset
/// of [`crate::i18n::TRANSLATABLE_PROPERTIES`].
pub const BUDGET_PROPERTIES: &[&str] = &["text", "placeholder_text"];

/// `anchors_preset` values (Godot's `Control.LayoutPreset` enum) whose
/// `anchor_left != anchor_right` - i.e. the control's horizontal extent
/// scales with its parent's width rather than being a fixed pixel size.
/// Only these four presets stretch *horizontally*; `LEFT_WIDE` (9),
/// `RIGHT_WIDE` (11), and `HCENTER_WIDE` (14) stretch only *vertically*
/// and keep `anchor_left == anchor_right`, so they are deliberately left
/// out here.
const HORIZONTAL_STRETCH_PRESETS: [i64; 4] = [
    10, // PRESET_TOP_WIDE
    12, // PRESET_BOTTOM_WIDE
    13, // PRESET_VCENTER_WIDE
    15, // PRESET_FULL_RECT
];

/// Where a node's available width came from - carried through to both the
/// text message and `--json` output so a reader can judge how much to
/// trust the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthSource {
    CustomMinimumSize,
    FixedOffsets,
}

impl WidthSource {
    pub fn label(self) -> &'static str {
        match self {
            WidthSource::CustomMinimumSize => "custom_minimum_size",
            WidthSource::FixedOffsets => "offset_left/offset_right",
        }
    }
}

/// One predicted text-overflow risk: a translatable string, the control it
/// lives in, and every number that went into the overflow decision.
#[derive(Debug, Clone, PartialEq)]
pub struct OverflowFinding {
    pub file: PathBuf,
    pub line: usize,
    pub string: String,
    pub node_path: String,
    pub node_type: String,
    pub property: &'static str,
    pub available_px: f64,
    pub source_px: f64,
    pub predicted_px: f64,
    pub expansion_percent: f64,
    pub font_size: f64,
    pub width_source: WidthSource,
}

impl OverflowFinding {
    /// Render the human-readable message half of a text-mode issue line
    /// (`sg check`'s `file:line: severity [code] message` shape - the
    /// `file:line: severity [code] ` prefix is added by the caller, same
    /// as every other `sg` command).
    pub fn message(&self) -> String {
        format!(
            "\"{}\" in {} \"{}\" may overflow: predicted ~{}px (source ~{}px +{}%) exceeds ~{}px available ({}, font_size {})",
            self.string,
            self.node_type,
            self.node_path,
            round_px(self.predicted_px),
            round_px(self.source_px),
            round_px(self.expansion_percent),
            round_px(self.available_px),
            self.width_source.label(),
            round_px(self.font_size),
        )
    }
}

/// Round a pixel/percent value to the nearest integer for display.
/// Applied consistently to every numeric field in both the text message
/// and `--json` output - the overflow *decision* itself always compares
/// full-precision `f64`s (see [`scan_document`]); rounding only ever
/// happens at render time, never before the comparison.
fn round_px(n: f64) -> i64 {
    n.round() as i64
}

/// The result of scanning a set of scene files for overflow risk: every
/// finding, in encounter order, plus one [`ScanError`] per file that could
/// not be read or parsed (same shape and meaning as
/// [`crate::i18n::ScanOutcome`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BudgetOutcome {
    pub findings: Vec<OverflowFinding>,
    pub errors: Vec<ScanError>,
}

/// Scan `files` (already-expanded scene file paths) for text-overflow
/// risk, applying `expansion_percent` and `default_font_size_px`.
pub fn scan(files: &[PathBuf], expansion_percent: f64, default_font_size_px: f64) -> BudgetOutcome {
    let mut outcome = BudgetOutcome::default();
    for file in files {
        let source = match crate::read_source(file) {
            Ok(s) => s,
            Err(message) => {
                outcome.errors.push(ScanError {
                    file: file.clone(),
                    message,
                });
                continue;
            }
        };
        match Document::parse(&source) {
            Ok(doc) => outcome
                .findings
                .extend(scan_document(&doc, file, expansion_percent, default_font_size_px)),
            Err(e) => outcome.errors.push(ScanError {
                file: file.clone(),
                message: format!("parse error: {e}"),
            }),
        }
    }
    outcome
}

/// Scan a single already-parsed `doc` for overflow risk, via the same
/// node-graph walk [`crate::i18n::scan_document`] uses for extraction.
fn scan_document(
    doc: &Document,
    scene_path: &Path,
    expansion_percent: f64,
    default_font_size_px: f64,
) -> Vec<OverflowFinding> {
    let sections = doc.sections();
    let graph = build_node_graph(&sections);

    let mut out = Vec::new();
    for &i in &graph.node_indices {
        let section = &sections[i];

        if is_autowrapping(section) {
            continue;
        }
        let Some((available_px, width_source)) = available_width(section) else {
            continue;
        };
        let font_size = resolved_font_size(section, default_font_size_px);

        let name = attr_str(section, "name").unwrap_or("");
        let parent_attr = attr_str(section, "parent");
        let node_path = node_full_path(name, parent_attr);
        let node_type = attr_str(section, "type").unwrap_or("").to_string();

        for property in &section.properties {
            let Some(&checked) = BUDGET_PROPERTIES.iter().find(|&&p| p == property.key) else {
                continue;
            };
            let Ok(value) = parse_complete(&property.raw_value) else {
                continue;
            };
            let Some(text) = value.as_str() else {
                continue;
            };
            if text.is_empty() {
                continue;
            }

            let source_px = estimate_text_width(text, font_size);
            let predicted_px = source_px * (1.0 + expansion_percent / 100.0);
            if predicted_px > available_px {
                out.push(OverflowFinding {
                    file: scene_path.to_path_buf(),
                    line: property.line,
                    string: text.to_string(),
                    node_path: node_path.clone(),
                    node_type: node_type.clone(),
                    property: checked,
                    available_px,
                    source_px,
                    predicted_px,
                    expansion_percent,
                    font_size,
                    width_source,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------
// Available width
// ---------------------------------------------------------------------

/// Determine a node's statically knowable available width in pixels, per
/// the precedence documented in this module's doc comment. `None` means
/// undeterminable - the caller must skip the node, not guess.
fn available_width(section: &SectionInfo) -> Option<(f64, WidthSource)> {
    if let Some(w) = property_raw(section, "custom_minimum_size").and_then(vector2_x) {
        if w > 0.0 {
            return Some((w, WidthSource::CustomMinimumSize));
        }
    }

    if is_stretching_horizontally(section) {
        return None;
    }

    let left = property_f64(section, "offset_left");
    let right = property_f64(section, "offset_right");
    if let (Some(l), Some(r)) = (left, right) {
        let w = (r - l).abs();
        if w > 0.0 {
            return Some((w, WidthSource::FixedOffsets));
        }
    }

    None
}

/// Whether `section`'s anchors make its horizontal extent scale with its
/// parent's size rather than being a fixed pixel width. `anchor_left`/
/// `anchor_right` take precedence when either is present (an absent one
/// defaults to Godot's own default of `0.0`); `anchors_preset` is only
/// consulted when *neither* anchor is written at all. A node with no
/// anchor information whatsoever defaults to `(0.0, 0.0)` - Godot's own
/// default `Control` anchoring - which is non-stretching.
fn is_stretching_horizontally(section: &SectionInfo) -> bool {
    let anchor_left = property_f64(section, "anchor_left");
    let anchor_right = property_f64(section, "anchor_right");
    if anchor_left.is_some() || anchor_right.is_some() {
        let l = anchor_left.unwrap_or(0.0);
        let r = anchor_right.unwrap_or(0.0);
        return (l - r).abs() > 1e-6;
    }

    if let Some(preset) = property_f64(section, "anchors_preset") {
        return HORIZONTAL_STRETCH_PRESETS.contains(&(preset.round() as i64));
    }

    false
}

/// Font size in px: the node's own `theme_override_font_sizes/font_size`
/// override if present, else `default_font_size_px`.
fn resolved_font_size(section: &SectionInfo, default_font_size_px: f64) -> f64 {
    property_f64(section, "theme_override_font_sizes/font_size").unwrap_or(default_font_size_px)
}

/// Whether `section` sets `autowrap_mode` to anything other than off
/// (`0`/`TextServer.AUTOWRAP_OFF`). A wrapping label overflows vertically,
/// not horizontally, so it is out of scope for this (horizontal) budget
/// check - not "fits", just not this tool's problem.
fn is_autowrapping(section: &SectionInfo) -> bool {
    property_f64(section, "autowrap_mode")
        .map(|mode| mode.round() as i64 != 0)
        .unwrap_or(false)
}

/// Read a body property's value as `f64` (accepting both `Value::Int` and
/// `Value::Float`). `None` if the property is absent, fails to parse, or
/// parses to a non-numeric value.
fn property_f64(section: &SectionInfo, key: &str) -> Option<f64> {
    let raw = property_raw(section, key)?;
    value_as_f64(&parse_complete(raw).ok()?)
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// Parse a `Vector2(x, y)` / `Vector2i(x, y)` property value's `x`
/// component. Rather than hand-rolling a fragile string-splitting parser
/// for Godot's constructor-call syntax, this reuses
/// [`scenegraph_core::parse_complete`] - the exact same variant-literal
/// parser every other `sg` property read already goes through - which
/// already tokenizes `Vector2(...)` into a
/// [`Value::Call`]`{name: "Vector2", args: [...]}` and already handles
/// internal whitespace, integers, floats, and negative numbers correctly;
/// re-deriving that grammar with a regex/split-based parser here would
/// risk silently disagreeing with it on some edge case. `None` for
/// anything that fails to parse, isn't a `Vector2`/`Vector2i` call, or has
/// no arguments.
fn vector2_x(raw: &str) -> Option<f64> {
    let value = parse_complete(raw).ok()?;
    let (name, args) = value.call()?;
    if name != "Vector2" && name != "Vector2i" {
        return None;
    }
    value_as_f64(args.first()?)
}

// ---------------------------------------------------------------------
// Text width estimation
// ---------------------------------------------------------------------

/// Estimate the rendered width, in pixels, of `text` at `font_size_px`.
/// Approximate by design (see the module doc comment): a static,
/// em-relative per-character width table, not a real font's metrics.
///
/// - CJK / full-width characters (Unicode ranges listed in
///   [`is_fullwidth`]) count as `1.0` em - a reasonable stand-in for any
///   full-width character in any commonly-used CJK font.
/// - Latin/other characters use [`char_width_em`]'s hand-picked table.
///
/// `width_px = sum(char_width_em(c) for c in text) * font_size_px`.
pub fn estimate_text_width(text: &str, font_size_px: f64) -> f64 {
    let em: f64 = text.chars().map(char_width_em).sum();
    em * font_size_px
}

/// Unicode ranges Godot's own CJK-aware text shaping treats as full-width:
/// CJK Unified Ideographs, Hiragana, Katakana, Hangul Syllables, CJK
/// Symbols/Punctuation, and the Fullwidth Forms block.
fn is_fullwidth(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0x3040..=0x309F // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0xAC00..=0xD7AF // Hangul Syllables
        | 0x3000..=0x303F // CJK Symbols and Punctuation
        | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
    )
}

/// One character's width, in em (font-size-relative) units. See the
/// module doc comment's "Estimating text width" table for the rationale
/// behind each bucket:
///
/// - full-width/CJK: `1.0`
/// - very narrow (`i l j f t I . , ' ! | :`): `0.3` - thin strokes or pure
///   punctuation with no horizontal bar
/// - narrow (`r s`, space): `0.35` - short strokes/counters, narrower than
///   an average lowercase letter but not as thin as the very-narrow set
/// - wide (`m w`): `0.9` - the two widest common lowercase letters
///   (two/three-stroke lowercase forms)
/// - uppercase (anything not already covered, e.g. not `I`): `0.65` -
///   uppercase letters run noticeably wider than lowercase in most
///   proportional fonts
/// - digit: `0.55` - most proportional fonts give digits a shared
///   (tabular-ish) width narrower than a typical uppercase letter
/// - default lowercase / unknown or other character (accented Latin,
///   Cyrillic, generic punctuation, emoji, ...): `0.5` - an unremarkable
///   proportional-font average, used as the catch-all so an unrecognized
///   character never contributes zero width
fn char_width_em(c: char) -> f64 {
    if is_fullwidth(c) {
        return 1.0;
    }
    match c {
        'i' | 'l' | 'j' | 'f' | 't' | 'I' | '.' | ',' | '\'' | '!' | '|' | ':' => 0.3,
        'r' | 's' | ' ' => 0.35,
        'm' | 'w' => 0.9,
        _ if c.is_ascii_uppercase() => 0.65,
        _ if c.is_ascii_digit() => 0.55,
        _ if c.is_ascii_lowercase() => 0.5,
        _ => 0.5,
    }
}

// ---------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------

fn finding_json(f: &OverflowFinding) -> String {
    format!(
        concat!(
            "{{\"file\":\"{}\",\"line\":{},\"severity\":\"warning\",\"code\":\"{}\",",
            "\"string\":\"{}\",\"node_path\":\"{}\",\"node_type\":\"{}\",\"property\":\"{}\",",
            "\"available_px\":{},\"source_px\":{},\"predicted_px\":{},",
            "\"expansion_percent\":{},\"font_size\":{},\"width_source\":\"{}\"}}"
        ),
        crate::json::escape(&f.file.display().to_string()),
        f.line,
        CODE,
        crate::json::escape(&f.string),
        crate::json::escape(&f.node_path),
        crate::json::escape(&f.node_type),
        f.property,
        round_px(f.available_px),
        round_px(f.source_px),
        round_px(f.predicted_px),
        round_px(f.expansion_percent),
        round_px(f.font_size),
        f.width_source.label(),
    )
}

/// Render `findings` as a single JSON array, in the same hand-rolled style
/// as [`crate::json::issues_array`] (no serde - the shape is tiny and
/// fixed).
pub fn render_json(findings: &[OverflowFinding]) -> String {
    let mut out = String::from("[");
    for (idx, f) in findings.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&finding_json(f));
    }
    out.push(']');
    out
}

// ---------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------

/// Run `sg i18n budget`: expand `paths` the same way `sg check` does, scan
/// every scene file found, and print either text lines (`sg check`'s
/// `file:line: severity [code] message` shape) or a `--json` array.
///
/// Exit code: `0` no overflow risk found, `1` at least one found, `2` at
/// least one input file failed to read or parse (matching `sg check`/
/// `sg i18n extract`'s parse-error exit code; this takes priority over
/// `1` the same way it does there).
pub fn run(paths: &[PathBuf], expansion_percent: u32, default_font_size: u32, json: bool) -> ExitCode {
    let files = collect_target_files(paths);
    let outcome = scan(&files, f64::from(expansion_percent), f64::from(default_font_size));

    for err in &outcome.errors {
        eprintln!("error: {}: {}", err.file.display(), err.message);
    }

    if json {
        println!("{}", render_json(&outcome.findings));
    } else {
        for finding in &outcome.findings {
            println!(
                "{}:{}: warning [{}] {}",
                finding.file.display(),
                finding.line,
                CODE,
                finding.message()
            );
        }
    }

    if !outcome.errors.is_empty() {
        ExitCode::from(2)
    } else if !outcome.findings.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section_from(src: &str) -> SectionInfo {
        let doc = Document::parse(src).unwrap();
        doc.sections().into_iter().nth(1).expect("expected a node section")
    }

    // -----------------------------------------------------------------
    // estimate_text_width
    // -----------------------------------------------------------------

    #[test]
    fn wide_characters_are_wider_than_very_narrow_ones() {
        assert!(estimate_text_width("WWWW", 16.0) > estimate_text_width("iiii", 16.0));
    }

    #[test]
    fn cjk_string_width_equals_char_count_times_font_size() {
        let text = "こんにちは"; // 5 hiragana characters
        assert_eq!(estimate_text_width(text, 16.0), 5.0 * 16.0);
    }

    #[test]
    fn width_scales_linearly_with_font_size() {
        let text = "Hello, world!";
        let at_16 = estimate_text_width(text, 16.0);
        let at_32 = estimate_text_width(text, 32.0);
        assert_eq!(at_32, at_16 * 2.0);
    }

    #[test]
    fn empty_string_has_zero_width() {
        assert_eq!(estimate_text_width("", 16.0), 0.0);
    }

    #[test]
    fn pinned_em_widths_for_each_bucket() {
        // very narrow
        assert_eq!(char_width_em('i'), 0.3);
        assert_eq!(char_width_em('I'), 0.3);
        assert_eq!(char_width_em('.'), 0.3);
        // narrow
        assert_eq!(char_width_em('s'), 0.35);
        assert_eq!(char_width_em(' '), 0.35);
        // wide
        assert_eq!(char_width_em('m'), 0.9);
        assert_eq!(char_width_em('w'), 0.9);
        // uppercase default (not in the very-narrow set)
        assert_eq!(char_width_em('M'), 0.65);
        assert_eq!(char_width_em('W'), 0.65);
        // digit
        assert_eq!(char_width_em('5'), 0.55);
        // default lowercase
        assert_eq!(char_width_em('a'), 0.5);
        // unknown/other (accented Latin) defaults to the same as lowercase
        assert_eq!(char_width_em('é'), 0.5);
        // full-width / CJK
        assert_eq!(char_width_em('あ'), 1.0);
        assert_eq!(char_width_em('漢'), 1.0);
        assert_eq!(char_width_em('한'), 1.0);
    }

    #[test]
    fn estimate_text_width_sums_the_per_char_table_exactly() {
        // "Settings" = S(.65) e(.5) t(.3) t(.3) i(.3) n(.5) g(.5) s(.35) = 3.40 em
        let width = estimate_text_width("Settings", 16.0);
        assert!((width - 54.4).abs() < 1e-9, "got {width}");
    }

    // -----------------------------------------------------------------
    // vector2_x
    // -----------------------------------------------------------------

    #[test]
    fn vector2_x_parses_integer_components() {
        assert_eq!(vector2_x("Vector2(120, 40)"), Some(120.0));
    }

    #[test]
    fn vector2_x_parses_float_components() {
        assert_eq!(vector2_x("Vector2(120.5, 40.0)"), Some(120.5));
    }

    #[test]
    fn vector2_x_tolerates_extra_whitespace() {
        assert_eq!(vector2_x("Vector2( 120 ,  40 )"), Some(120.0));
    }

    #[test]
    fn vector2_x_accepts_the_integer_vector_variant() {
        assert_eq!(vector2_x("Vector2i(120, 40)"), Some(120.0));
    }

    #[test]
    fn vector2_x_rejects_a_different_constructor() {
        assert_eq!(vector2_x("Vector3(1, 2, 3)"), None);
    }

    #[test]
    fn vector2_x_rejects_malformed_text() {
        assert_eq!(vector2_x("not a vector"), None);
        assert_eq!(vector2_x("Vector2(120"), None);
        assert_eq!(vector2_x("Vector2()"), None);
    }

    // -----------------------------------------------------------------
    // available_width precedence
    // -----------------------------------------------------------------

    #[test]
    fn custom_minimum_size_wins_over_conflicting_offsets() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "custom_minimum_size = Vector2(80, 32)\n",
            "offset_left = 0.0\n",
            "offset_right = 500.0\n",
        ));
        assert_eq!(available_width(&section), Some((80.0, WidthSource::CustomMinimumSize)));
    }

    #[test]
    fn zero_custom_minimum_size_falls_through_to_offsets() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "custom_minimum_size = Vector2(0, 32)\n",
            "offset_left = 10.0\n",
            "offset_right = 90.0\n",
        ));
        assert_eq!(available_width(&section), Some((80.0, WidthSource::FixedOffsets)));
    }

    #[test]
    fn negative_custom_minimum_size_falls_through_to_offsets() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "custom_minimum_size = Vector2(-10, 32)\n",
            "offset_left = 10.0\n",
            "offset_right = 90.0\n",
        ));
        assert_eq!(available_width(&section), Some((80.0, WidthSource::FixedOffsets)));
    }

    #[test]
    fn offsets_used_when_anchors_absent_default_to_non_stretching() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "offset_left = 10.0\n",
            "offset_right = 90.0\n",
        ));
        assert_eq!(available_width(&section), Some((80.0, WidthSource::FixedOffsets)));
    }

    #[test]
    fn offsets_used_when_anchors_explicitly_equal() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "anchor_left = 0.5\n",
            "anchor_right = 0.5\n",
            "offset_left = -40.0\n",
            "offset_right = 40.0\n",
        ));
        assert_eq!(available_width(&section), Some((80.0, WidthSource::FixedOffsets)));
    }

    #[test]
    fn undeterminable_when_anchors_stretch_horizontally() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Label\"]\n",
            "anchor_right = 1.0\n", // anchor_left defaults to 0.0, so 0.0 != 1.0
            "offset_left = 8.0\n",
            "offset_right = -8.0\n",
        ));
        assert_eq!(available_width(&section), None);
    }

    #[test]
    fn undeterminable_when_anchors_preset_is_a_horizontal_stretch_preset() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Label\"]\n",
            "anchors_preset = 15\n",
            "offset_left = 8.0\n",
            "offset_right = -8.0\n",
        ));
        assert_eq!(available_width(&section), None);
    }

    #[test]
    fn offsets_used_when_anchors_preset_is_not_a_stretch_preset() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "anchors_preset = 0\n",
            "offset_left = 10.0\n",
            "offset_right = 90.0\n",
        ));
        assert_eq!(available_width(&section), Some((80.0, WidthSource::FixedOffsets)));
    }

    #[test]
    fn undeterminable_with_no_geometry_at_all() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "text = \"Hi\"\n",
        ));
        assert_eq!(available_width(&section), None);
    }

    #[test]
    fn undeterminable_when_offsets_yield_zero_width() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "offset_left = 10.0\n",
            "offset_right = 10.0\n",
        ));
        assert_eq!(available_width(&section), None);
    }

    // -----------------------------------------------------------------
    // font size resolution
    // -----------------------------------------------------------------

    #[test]
    fn font_size_reads_theme_override_when_present() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "theme_override_font_sizes/font_size = 24\n",
        ));
        assert_eq!(resolved_font_size(&section, 16.0), 24.0);
    }

    #[test]
    fn font_size_falls_back_to_the_given_default_when_absent() {
        let section = section_from(concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"B\" type=\"Button\"]\n",
            "text = \"Hi\"\n",
        ));
        assert_eq!(resolved_font_size(&section, 16.0), 16.0);
        assert_eq!(resolved_font_size(&section, 24.0), 24.0);
    }

    // -----------------------------------------------------------------
    // autowrap
    // -----------------------------------------------------------------

    #[test]
    fn autowrap_mode_nonzero_is_skipped_even_with_a_determinable_width() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"Hint\" type=\"Label\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(10, 60)\n",
            "autowrap_mode = 3\n",
            "text = \"This text is far too long for a ten pixel wide label\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 40.0, 16.0);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn autowrap_mode_off_does_not_suppress_a_real_overflow() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"Hint\" type=\"Label\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(10, 60)\n",
            "autowrap_mode = 0\n",
            "text = \"This text is far too long for a ten pixel wide label\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 40.0, 16.0);
        assert_eq!(findings.len(), 1);
    }

    // -----------------------------------------------------------------
    // overflow decision boundary: predicted > available, strictly
    // -----------------------------------------------------------------

    #[test]
    fn predicted_exactly_equal_to_available_does_not_warn() {
        // "i" at font_size 16 -> 0.3 * 16 = 4.8px. expansion 0% keeps
        // predicted == source exactly, so an available width of exactly
        // 4.8 must NOT warn (the boundary is strict '>', not '>=').
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"B\" type=\"Button\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(4.8, 20)\n",
            "text = \"i\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 0.0, 16.0);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn predicted_one_unit_over_available_warns() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"B\" type=\"Button\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(4.7, 20)\n",
            "text = \"i\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 0.0, 16.0);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn predicted_below_available_does_not_warn() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"B\" type=\"Button\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(100, 20)\n",
            "text = \"OK\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 40.0, 16.0);
        assert!(findings.is_empty(), "{findings:?}");
    }

    // -----------------------------------------------------------------
    // scan_document: end-to-end field wiring
    // -----------------------------------------------------------------

    #[test]
    fn finding_carries_node_path_type_and_all_numbers() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Menu\" type=\"Control\"]\n",
            "\n",
            "[node name=\"VBox\" type=\"VBoxContainer\" parent=\".\"]\n",
            "\n",
            "[node name=\"SettingsButton\" type=\"Button\" parent=\"VBox\"]\n",
            "custom_minimum_size = Vector2(70, 32)\n",
            "text = \"Settings\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 40.0, 16.0);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.string, "Settings");
        assert_eq!(f.node_path, "VBox/SettingsButton");
        assert_eq!(f.node_type, "Button");
        assert_eq!(f.property, "text");
        assert_eq!(f.line, 9);
        assert_eq!(f.available_px, 70.0);
        assert!((f.source_px - 54.4).abs() < 1e-9);
        assert!((f.predicted_px - 76.16).abs() < 1e-9);
        assert_eq!(f.expansion_percent, 40.0);
        assert_eq!(f.font_size, 16.0);
        assert_eq!(f.width_source, WidthSource::CustomMinimumSize);
        assert_eq!(
            f.message(),
            "\"Settings\" in Button \"VBox/SettingsButton\" may overflow: predicted ~76px (source ~54px +40%) exceeds ~70px available (custom_minimum_size, font_size 16)"
        );
    }

    #[test]
    fn placeholder_text_is_checked_the_same_way_as_text() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"NameInput\" type=\"LineEdit\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(10, 20)\n",
            "placeholder_text = \"Enter your full name here\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 40.0, 16.0);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].property, "placeholder_text");
    }

    #[test]
    fn tooltip_text_and_title_are_never_checked() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"B\" type=\"Button\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(5, 20)\n",
            "tooltip_text = \"This tooltip text is extremely long and would overflow anything\"\n",
            "\n",
            "[node name=\"W\" type=\"Window\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(5, 20)\n",
            "title = \"This window title is extremely long and would overflow anything\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 40.0, 16.0);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn empty_string_is_never_a_finding_even_with_zero_available_width() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"B\" type=\"Button\" parent=\".\"]\n",
            "custom_minimum_size = Vector2(5, 20)\n",
            "text = \"\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let findings = scan_document(&doc, Path::new("scene.tscn"), 40.0, 16.0);
        assert!(findings.is_empty(), "{findings:?}");
    }

    // -----------------------------------------------------------------
    // JSON rendering
    // -----------------------------------------------------------------

    #[test]
    fn render_json_of_empty_findings_is_an_empty_array() {
        assert_eq!(render_json(&[]), "[]");
    }

    #[test]
    fn render_json_includes_every_documented_field() {
        let finding = OverflowFinding {
            file: PathBuf::from("menu.tscn"),
            line: 9,
            string: "Settings".to_string(),
            node_path: "VBox/SettingsButton".to_string(),
            node_type: "Button".to_string(),
            property: "text",
            available_px: 70.0,
            source_px: 54.4,
            predicted_px: 76.16,
            expansion_percent: 40.0,
            font_size: 16.0,
            width_source: WidthSource::CustomMinimumSize,
        };
        let json = render_json(&[finding]);
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
        assert!(json.contains("\"code\":\"i18n-text-overflow\""), "{json}");
        assert!(json.contains("\"severity\":\"warning\""), "{json}");
        assert!(json.contains("\"string\":\"Settings\""), "{json}");
        assert!(json.contains("\"node_path\":\"VBox/SettingsButton\""), "{json}");
        assert!(json.contains("\"node_type\":\"Button\""), "{json}");
        assert!(json.contains("\"available_px\":70"), "{json}");
        assert!(json.contains("\"source_px\":54"), "{json}");
        assert!(json.contains("\"predicted_px\":76"), "{json}");
        assert!(json.contains("\"expansion_percent\":40"), "{json}");
        assert!(json.contains("\"font_size\":16"), "{json}");
        assert!(json.contains("\"width_source\":\"custom_minimum_size\""), "{json}");
    }
}
