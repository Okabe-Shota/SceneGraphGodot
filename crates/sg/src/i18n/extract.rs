//! `sg i18n extract`: scans scene files for the v1 translatable-string set
//! ([`crate::i18n::TRANSLATABLE_PROPERTIES`]) via the shared scan layer in
//! [`crate::i18n`], and formats the result as a gettext PO file (default)
//! or a translator-facing CSV.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::i18n::{scan, TranslatableString};
use crate::paths::collect_target_files;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Gettext PO: one entry per unique string, merging every occurrence
    /// into shared `#.`/`#:` comment/reference lines. The default -
    /// Godot can import `.po` translations directly, and gettext tooling
    /// (msgmerge, Poedit, Weblate, ...) already understands the format.
    Po,
    /// `key,source,context`: one row per occurrence (not merged like PO),
    /// for reviewing in a spreadsheet with full context on every use.
    Csv,
}

/// Run `sg i18n extract`: expand `paths` the same way `sg check` does,
/// scan every scene file found, and write the formatted result to
/// `output` (or stdout when `None`).
///
/// Exit code: `0` every input file scanned cleanly (regardless of how
/// many - or how few - translatable strings were found: an empty result
/// is not a failure), `2` at least one input file failed to read or
/// parse (matching `sg check`/`sg fix`'s parse-error exit code), `1` the
/// output could not be written to `--output`.
pub fn run(paths: &[PathBuf], format: Format, output: Option<&Path>) -> ExitCode {
    let files = collect_target_files(paths);
    let outcome = scan(&files);

    for err in &outcome.errors {
        eprintln!("error: {}: {}", err.file.display(), err.message);
    }

    let rendered = match format {
        Format::Po => render_po(&outcome.records),
        Format::Csv => render_csv(&outcome.records),
    };

    match output {
        Some(path) => {
            if let Err(e) = fs::write(path, &rendered) {
                eprintln!("error: failed to write '{}': {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
        None => print!("{rendered}"),
    }

    if outcome.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

/// The reference this record's occurrence resolves to: `res_path` when
/// known, otherwise `scene_path` (forward-slash-separated regardless of
/// host path separator), followed by `:node_path` - e.g.
/// `res://ui/main_menu.tscn:VBox/StartButton`. `pub(crate)` rather than
/// private: `sg i18n shots` reuses this exact computation for its own
/// per-string reference column, so the whole `sg i18n` family always
/// agrees on what a string's reference string looks like.
pub(crate) fn reference(record: &TranslatableString) -> String {
    let base = record
        .res_path
        .clone()
        .unwrap_or_else(|| record.scene_path.to_string_lossy().replace('\\', "/"));
    format!("{base}:{}", record.node_path)
}

/// Group `records` by exact text (the msgid), preserving first-occurrence
/// order - see [`render_po`]'s doc comment for why this ordering was
/// chosen over sorting by msgid.
fn group_by_text(records: &[TranslatableString]) -> Vec<(&str, Vec<&TranslatableString>)> {
    let mut order: Vec<&str> = Vec::new();
    let mut groups: std::collections::HashMap<&str, Vec<&TranslatableString>> = std::collections::HashMap::new();
    for record in records {
        let key = record.text.as_str();
        if !groups.contains_key(key) {
            order.push(key);
        }
        groups.entry(key).or_default().push(record);
    }
    order
        .into_iter()
        .map(|key| (key, groups.remove(key).unwrap()))
        .collect()
}

/// Escape a string for use as a PO `msgid`/`msgstr` literal: backslash,
/// double-quote, newline, tab, and carriage return. gettext PO strings are
/// always written as a single quoted line (an embedded `\n` stays an
/// escape sequence, it is not turned into an actual line break), so no
/// line-splitting is needed on top of this.
fn escape_po_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Render `records` as a minimal, valid gettext PO file.
///
/// Ordering: entries are emitted in first-occurrence order (the order
/// their msgid was first seen while scanning), not sorted by msgid. This
/// keeps the file's entry order following the scenes as a translator
/// would encounter them (file order, then top-to-bottom through each
/// scene's nodes) instead of an alphabetical shuffle that would put
/// unrelated strings from unrelated screens next to each other; it is
/// still fully deterministic (a re-run over unchanged input produces
/// byte-identical output) because scanning itself is deterministic.
///
/// Merging: every occurrence of the same exact text becomes one entry,
/// accumulating one `#.` extracted-comment line (`Type: ... | Screen: ...
/// | Property: ...`) and one `#:` reference line
/// (`res://path/to/scene.tscn:Node/Path`) per occurrence - both are
/// standard, repeatable gettext comment kinds.
///
/// Comment grouping: within one entry, every `#.` line is emitted before
/// every `#:` line (occurrence i's comment and reference stay positionally
/// aligned across the two groups) - never interleaved per-occurrence.
/// This matches the gettext PO convention of grouping by comment *kind*
/// (all extracted comments, then all references), which is what
/// `msgmerge`/`msgcat` normalize to and what PO-consuming tools (Poedit,
/// Crowdin, Weblate) expect; interleaving risks spurious diffs against
/// that normalized form and undermines the interoperability PO was chosen
/// for in the first place. Duplicate `#.` lines (occurrences that happen
/// to share the same Type/Screen/Property) are deliberately *not*
/// collapsed - one `#.` line per occurrence is kept, so the number of
/// `#.` lines always equals the number of `#:` lines and no occurrence's
/// context is ever silently merged away.
fn render_po(records: &[TranslatableString]) -> String {
    let mut out = String::new();
    out.push_str("msgid \"\"\n");
    out.push_str("msgstr \"\"\n");
    out.push_str("\"Content-Type: text/plain; charset=UTF-8\\n\"\n");
    out.push_str("\"Content-Transfer-Encoding: 8bit\\n\"\n");

    for (text, occurrences) in group_by_text(records) {
        out.push('\n');
        for occ in &occurrences {
            out.push_str(&format!(
                "#. Type: {} | Screen: {} | Property: {}\n",
                occ.node_type, occ.screen, occ.property
            ));
        }
        for occ in &occurrences {
            out.push_str(&format!("#: {}\n", reference(occ)));
        }
        out.push_str(&format!("msgid \"{}\"\n", escape_po_string(text)));
        out.push_str("msgstr \"\"\n");
    }
    out
}

/// Quote a CSV field per RFC 4180: wrap in double quotes (doubling any
/// internal double quote) whenever the field contains a comma, a double
/// quote, or a newline; otherwise leave it bare.
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Render `records` as a `key,source,context` CSV.
///
/// Shape chosen over Godot's native `keys,<locale>` import shape: this
/// CSV's job (per README.md, "sg i18n extract") is translator-facing
/// spreadsheet review - the pain point it solves is "translators get no
/// context" - not re-importing translations back into Godot (`.po`,
/// which Godot *can* import directly, is the primary format for that).
/// An explicit `source` column also does not presuppose a source locale
/// the way a literal `en` column would. `key` and `source` are identical
/// in v1, since `.tscn` UI text has no separate translation-key concept
/// yet - the string literal itself is the key, exactly as it is in the
/// PO output's `msgid` - but keeping both columns (rather than
/// collapsing to one) leaves room for a future distinct key scheme
/// without changing the CSV's shape.
///
/// One row per *occurrence*, not merged by text like PO: unlike a PO
/// entry (which gettext tooling expects merged by msgid), a translator
/// reviewing a spreadsheet benefits from seeing every occurrence's own
/// context in place, since identical source text can legitimately need
/// different translations depending on where it appears.
fn render_csv(records: &[TranslatableString]) -> String {
    let mut out = String::from("key,source,context\n");
    for record in records {
        let context = format!(
            "Type: {} | Screen: {} | Property: {} | Ref: {}",
            record.node_type,
            record.screen,
            record.property,
            reference(record)
        );
        out.push_str(&csv_field(&record.text));
        out.push(',');
        out.push_str(&csv_field(&record.text));
        out.push(',');
        out.push_str(&csv_field(&context));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------
// PO reader
// ---------------------------------------------------------------------

/// `msgid -> msgstr`, as read by [`parse_po`].
pub(crate) type PoMap = HashMap<String, String>;

/// Parse a minimal gettext PO file into a `msgid -> msgstr` map - the
/// mirror image of [`render_po`]: same escape set (`\\ \" \n \t \r`, via
/// [`parse_po_string_literal`]), same multi-line continuation-string
/// convention (a bare quoted line immediately following a `msgid`/
/// `msgstr` line concatenates onto it, exactly how `render_po`'s own
/// header emits its two `"Content-Type: ...\n"`-style lines after
/// `msgstr ""`). Used by `sg i18n check`'s untranslated gate to load a
/// translator-filled `--against` file.
///
/// Policies:
/// - The header entry (empty `msgid`) is always skipped - it is never a
///   real source string, so it is never a key in the returned map.
/// - Comment lines (`#`, `#.`, `#:`, `#,`, ...) and blank lines are
///   ignored wherever they appear between entries.
/// - Duplicate `msgid`s: **the last occurrence in the file wins** - a
///   later entry overwrites an earlier one with the same `msgid` (a plain
///   `HashMap::insert` per entry, applied top-to-bottom, gives this for
///   free). This matches treating a hand-edited file's most recent copy
///   of a duplicated entry as the intended one.
/// - A malformed entry - an unterminated string, an unrecognized escape,
///   trailing content after a string's closing quote, or a `msgid` line
///   with no following `msgstr` line - is **skipped entirely** rather
///   than making the whole file unreadable or panicking: parsing resumes
///   at the next blank-line-separated entry (see [`skip_to_blank`]).
///   This mirrors the project's general tolerant-parsing stance elsewhere
///   (`scenegraph_core::Document::parse_tolerant`) - a single corrupted
///   hand-edit should not sink an otherwise-usable translation file.
///
/// A present-but-empty `msgstr` is kept in the map as `""`, not treated
/// the same as "absent": callers (`sg i18n check`) need to distinguish
/// "never extracted" (key absent) from "extracted but not yet translated"
/// (key present, value empty).
pub(crate) fn parse_po(source: &str) -> PoMap {
    let mut map = PoMap::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }

        let (keyword, rest) = split_keyword(line);
        if keyword != "msgid" {
            // Not an entry-opening line (stray text, or an unsupported
            // keyword such as `msgid_plural` - not needed for `sg i18n`,
            // whose PO files are always single-target). Skip and keep
            // looking for the next `msgid`.
            i += 1;
            continue;
        }

        let Some(mut msgid) = parse_po_string_literal(rest) else {
            i = skip_to_blank(&lines, i + 1);
            continue;
        };
        i += 1;
        i = consume_continuation(&lines, i, &mut msgid);

        if i >= lines.len() {
            break; // malformed: msgid with no msgstr at all, then EOF
        }
        let (keyword2, rest2) = split_keyword(lines[i].trim());
        if keyword2 != "msgstr" {
            i = skip_to_blank(&lines, i);
            continue;
        }
        let Some(mut msgstr) = parse_po_string_literal(rest2) else {
            i = skip_to_blank(&lines, i + 1);
            continue;
        };
        i += 1;
        i = consume_continuation(&lines, i, &mut msgstr);

        if !msgid.is_empty() {
            map.insert(msgid, msgstr); // last occurrence wins
        }
    }
    map
}

/// Split `line` (already trimmed) into its first whitespace-delimited
/// token and the (start-trimmed) remainder of the line - used to
/// recognize the `msgid`/`msgstr` keywords without also matching a
/// longer keyword that merely starts with the same letters (e.g.
/// `msgid_plural`).
fn split_keyword(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(idx) => (&line[..idx], line[idx..].trim_start()),
        None => (line, ""),
    }
}

/// Consume every subsequent bare-quoted continuation line (no keyword
/// prefix) starting at `i`, appending each one's decoded content to
/// `into` in order. Stops at the first line that is not a well-formed,
/// self-contained quoted literal (including a line that is not quoted at
/// all) or at end of input. Returns the index of the first line not
/// consumed.
fn consume_continuation(lines: &[&str], mut i: usize, into: &mut String) -> usize {
    while i < lines.len() {
        let l = lines[i].trim();
        if !l.starts_with('"') {
            break;
        }
        match parse_po_string_literal(l) {
            Some(s) => {
                into.push_str(&s);
                i += 1;
            }
            None => break,
        }
    }
    i
}

/// Recover from a malformed entry by advancing past it: skip forward to
/// the next blank line (exclusive - the blank line itself is left for the
/// outer loop, which already skips blank lines) or to end of input. Note
/// this may also skip a well-formed entry that immediately follows the
/// malformed one with no blank-line separator - an edge case that never
/// arises in any well-formed PO file, since entries are always
/// blank-line-separated (exactly how [`render_po`] itself always emits
/// them).
fn skip_to_blank(lines: &[&str], mut i: usize) -> usize {
    while i < lines.len() && !lines[i].trim().is_empty() {
        i += 1;
    }
    i
}

/// Parse a single PO string literal (`"..."`, with backslash escapes)
/// that must consume the entirety of `line` (already trimmed). Mirrors
/// [`escape_po_string`] in reverse: `\\` `\"` `\n` `\t` `\r`. `None` for
/// anything that is not a complete, well-formed literal: a missing
/// opening or closing quote, an unterminated string, a trailing
/// (unescaped) backslash, an unrecognized escape sequence, or trailing
/// content after the closing quote.
fn parse_po_string_literal(line: &str) -> Option<String> {
    let mut chars = line.chars();
    if chars.next() != Some('"') {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    let mut closed = false;
    for c in chars.by_ref() {
        if escaped {
            out.push(match c {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                _ => return None, // unrecognized escape: malformed
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            closed = true;
            break;
        } else {
            out.push(c);
        }
    }
    if !closed || escaped {
        return None;
    }
    if chars.next().is_some() {
        return None; // trailing content after the closing quote
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(text: &str, node_path: &str, property: &str) -> TranslatableString {
        TranslatableString {
            text: text.to_string(),
            scene_path: PathBuf::from("ui/main_menu.tscn"),
            res_path: Some("res://ui/main_menu.tscn".to_string()),
            node_path: node_path.to_string(),
            node_type: "Button".to_string(),
            screen: "MainMenu".to_string(),
            property: property.to_string(),
            line: 1,
        }
    }

    // -----------------------------------------------------------------
    // PO escaping
    // -----------------------------------------------------------------

    #[test]
    fn escapes_quote_backslash_and_newline() {
        assert_eq!(escape_po_string("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    #[test]
    fn escapes_tab_and_carriage_return() {
        assert_eq!(escape_po_string("a\tb\rc"), "a\\tb\\rc");
    }

    #[test]
    fn plain_ascii_is_unescaped() {
        assert_eq!(escape_po_string("Start Game"), "Start Game");
    }

    // -----------------------------------------------------------------
    // PO rendering: header, merge, ordering
    // -----------------------------------------------------------------

    #[test]
    fn empty_input_yields_only_the_header() {
        let po = render_po(&[]);
        assert_eq!(
            po,
            concat!(
                "msgid \"\"\n",
                "msgstr \"\"\n",
                "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
                "\"Content-Transfer-Encoding: 8bit\\n\"\n",
            )
        );
    }

    #[test]
    fn header_declares_utf8_and_8bit_encoding() {
        let po = render_po(&[]);
        assert!(po.contains("Content-Type: text/plain; charset=UTF-8"));
        assert!(po.contains("Content-Transfer-Encoding: 8bit"));
    }

    #[test]
    fn single_entry_has_one_comment_and_one_reference() {
        let po = render_po(&[record("Start Game", "VBox/StartButton", "text")]);
        assert!(po.contains("#. Type: Button | Screen: MainMenu | Property: text\n"));
        assert!(po.contains("#: res://ui/main_menu.tscn:VBox/StartButton\n"));
        assert!(po.contains("msgid \"Start Game\"\n"));
        assert!(po.contains("msgstr \"\"\n"));
    }

    #[test]
    fn duplicate_text_merges_into_one_entry_with_multiple_references() {
        let records = vec![
            record("Cancel", "VBox/CancelButton", "text"),
            record("Cancel", "VBox/CloseButton", "text"),
        ];
        let po = render_po(&records);
        // Exactly one msgid line for the shared text...
        assert_eq!(po.matches("msgid \"Cancel\"").count(), 1, "{po}");
        // ...but both references are present...
        assert!(po.contains("#: res://ui/main_menu.tscn:VBox/CancelButton\n"), "{po}");
        assert!(po.contains("#: res://ui/main_menu.tscn:VBox/CloseButton\n"), "{po}");
        assert_eq!(po.matches("#:").count(), 2, "{po}");
        // ...and one `#.` comment per occurrence is kept, even though both
        // are identical here (same Type/Screen/Property): collapsing
        // identical extracted comments is optional per gettext convention
        // and deliberately not done, so the `#.` count always matches the
        // `#:` count and no occurrence's context is ever silently merged
        // away.
        assert_eq!(po.matches("#.").count(), 2, "{po}");
    }

    #[test]
    fn duplicate_text_groups_all_comments_before_all_references_not_interleaved() {
        // Exact byte match against the gettext-conforming shape: every
        // `#.` line for the entry, then every `#:` line - never
        // interleaved per-occurrence. This is the shape `msgmerge`/
        // `msgcat` normalize to and what Poedit/Crowdin/Weblate expect.
        let records = vec![
            record("Cancel", "VBox/CancelButton", "text"),
            record("Cancel", "VBox/CloseButton", "text"),
        ];
        let po = render_po(&records);
        let entry = concat!(
            "#. Type: Button | Screen: MainMenu | Property: text\n",
            "#. Type: Button | Screen: MainMenu | Property: text\n",
            "#: res://ui/main_menu.tscn:VBox/CancelButton\n",
            "#: res://ui/main_menu.tscn:VBox/CloseButton\n",
            "msgid \"Cancel\"\n",
            "msgstr \"\"\n",
        );
        assert!(po.ends_with(entry), "{po}");

        // Positional cross-check: every `#.` line must precede every `#:`
        // line for this entry (not just "the last one written" - proves
        // there is no interleaving anywhere in the block).
        let last_dot = po.rfind("#.").expect("expected a #. line");
        let first_colon = po.find("#:").expect("expected a #: line");
        assert!(last_dot < first_colon, "every #. line must precede every #: line: {po}");
    }

    #[test]
    fn entries_are_emitted_in_first_occurrence_order_not_alphabetical() {
        let records = vec![
            record("Zebra", "A", "text"),
            record("Apple", "B", "text"),
            record("Zebra", "C", "text"),
        ];
        let po = render_po(&records);
        let zebra_pos = po.find("msgid \"Zebra\"").unwrap();
        let apple_pos = po.find("msgid \"Apple\"").unwrap();
        assert!(
            zebra_pos < apple_pos,
            "Zebra was seen first and must be emitted first: {po}"
        );
    }

    #[test]
    fn repeated_scan_produces_byte_identical_output() {
        let records = vec![record("Start Game", "VBox/StartButton", "text")];
        assert_eq!(render_po(&records), render_po(&records));
    }

    // -----------------------------------------------------------------
    // CSV
    // -----------------------------------------------------------------

    #[test]
    fn csv_header_only_for_empty_input() {
        assert_eq!(render_csv(&[]), "key,source,context\n");
    }

    #[test]
    fn csv_quotes_a_value_containing_a_comma_and_a_quote() {
        let csv = render_csv(&[record("Say \"hi\", friend", "VBox/Label", "text")]);
        let expected_field = "\"Say \"\"hi\"\", friend\"";
        assert!(csv.contains(expected_field), "{csv}");
        // The field appears twice (key and source columns).
        assert_eq!(csv.matches(expected_field).count(), 2, "{csv}");
    }

    #[test]
    fn csv_leaves_plain_fields_unquoted() {
        let csv = render_csv(&[record("Start Game", "VBox/StartButton", "text")]);
        assert!(csv.contains("Start Game,Start Game,"), "{csv}");
    }

    #[test]
    fn csv_emits_one_row_per_occurrence_not_merged() {
        let records = vec![
            record("Cancel", "VBox/CancelButton", "text"),
            record("Cancel", "VBox/CloseButton", "text"),
        ];
        let csv = render_csv(&records);
        assert_eq!(csv.lines().count(), 3, "header + two rows: {csv}");
    }

    #[test]
    fn csv_context_includes_type_screen_property_and_reference() {
        let csv = render_csv(&[record("Start Game", "VBox/StartButton", "text")]);
        assert!(
            csv.contains("Type: Button | Screen: MainMenu | Property: text"),
            "{csv}"
        );
        assert!(csv.contains("Ref: res://ui/main_menu.tscn:VBox/StartButton"), "{csv}");
    }

    // -----------------------------------------------------------------
    // reference()
    // -----------------------------------------------------------------

    #[test]
    fn reference_prefers_res_path_over_scene_path() {
        let r = record("x", "Node", "text");
        assert_eq!(reference(&r), "res://ui/main_menu.tscn:Node");
    }

    #[test]
    fn reference_falls_back_to_scene_path_without_a_res_path() {
        let mut r = record("x", "Node", "text");
        r.res_path = None;
        r.scene_path = PathBuf::from("scenes/menu.tscn");
        assert_eq!(reference(&r), "scenes/menu.tscn:Node");
    }

    // -----------------------------------------------------------------
    // PO reader (parse_po)
    // -----------------------------------------------------------------

    #[test]
    fn parses_a_single_translated_entry() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
            "\n",
            "msgid \"Start Game\"\n",
            "msgstr \"Spiel starten\"\n",
        );
        let map = parse_po(po);
        assert_eq!(map.get("Start Game").map(String::as_str), Some("Spiel starten"));
    }

    #[test]
    fn header_entry_with_empty_msgid_is_skipped() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
        );
        let map = parse_po(po);
        assert!(map.is_empty(), "{map:?}");
        assert!(!map.contains_key(""));
    }

    #[test]
    fn multi_line_continuation_strings_concatenate() {
        // Mirrors how render_po's own header emits msgstr "" followed by
        // bare-quoted continuation lines.
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\n",
            "msgid \"Long\"\n",
            "msgstr \"\"\n",
            "\"first part \"\n",
            "\"second part\"\n",
        );
        let map = parse_po(po);
        assert_eq!(map.get("Long").map(String::as_str), Some("first part second part"));
    }

    #[test]
    fn round_trips_every_escape_the_writer_escapes() {
        // Round-trip through the writer's own escaping (escape_po_string)
        // and back through the reader (parse_po) must reproduce the
        // original text exactly, for every escape the writer emits.
        let original = "a\"b\\c\nd\te\rf";
        let po = format!(
            "msgid \"\"\nmsgstr \"\"\n\nmsgid \"{}\"\nmsgstr \"ok\"\n",
            escape_po_string(original)
        );
        let map = parse_po(&po);
        assert_eq!(map.get(original).map(String::as_str), Some("ok"));
    }

    #[test]
    fn writer_output_parsed_back_yields_the_original_msgids() {
        let records = vec![
            record("Start Game", "VBox/StartButton", "text"),
            record("Say \"hi\", friend\nnewline", "VBox/Label", "text"),
        ];
        let po = render_po(&records);
        let map = parse_po(&po);
        assert!(map.contains_key("Start Game"), "{map:?}");
        assert!(map.contains_key("Say \"hi\", friend\nnewline"), "{map:?}");
        // extract's own writer always emits an empty msgstr - both keys
        // must be present with an empty translation, not absent.
        assert_eq!(map.get("Start Game").map(String::as_str), Some(""));
    }

    #[test]
    fn empty_msgstr_is_present_in_the_map_not_absent() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\n",
            "msgid \"Cancel\"\n",
            "msgstr \"\"\n",
        );
        let map = parse_po(po);
        assert_eq!(map.get("Cancel").map(String::as_str), Some(""));
    }

    #[test]
    fn a_msgid_never_written_is_simply_absent() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\n",
            "msgid \"Cancel\"\n",
            "msgstr \"Abbrechen\"\n",
        );
        let map = parse_po(po);
        assert!(!map.contains_key("Never Extracted"));
    }

    #[test]
    fn comment_lines_are_ignored() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\n",
            "#. Type: Button | Screen: MainMenu | Property: text\n",
            "#: res://main_menu.tscn:VBox/StartButton\n",
            "#, fuzzy\n",
            "# a plain translator comment\n",
            "msgid \"Start Game\"\n",
            "msgstr \"Spiel starten\"\n",
        );
        let map = parse_po(po);
        assert_eq!(map.get("Start Game").map(String::as_str), Some("Spiel starten"));
    }

    #[test]
    fn duplicate_msgid_last_occurrence_wins() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\n",
            "msgid \"Cancel\"\n",
            "msgstr \"Abbrechen\"\n",
            "\n",
            "msgid \"Cancel\"\n",
            "msgstr \"Zweite Übersetzung\"\n",
        );
        let map = parse_po(po);
        assert_eq!(map.get("Cancel").map(String::as_str), Some("Zweite Übersetzung"));
    }

    #[test]
    fn malformed_entry_with_an_unterminated_string_is_skipped_not_fatal() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\n",
            "msgid \"Broken\n", // missing closing quote: malformed
            "msgstr \"whatever\"\n",
            "\n",
            "msgid \"Fine\"\n",
            "msgstr \"OK\"\n",
        );
        let map = parse_po(po);
        assert!(!map.contains_key("Broken"), "{map:?}");
        // Parsing must recover and still pick up the entry that follows.
        assert_eq!(map.get("Fine").map(String::as_str), Some("OK"));
    }

    #[test]
    fn malformed_entry_missing_msgstr_entirely_is_skipped_not_fatal() {
        let po = concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\n",
            "msgid \"NoTranslationLineAtAll\"\n",
            "\n",
            "msgid \"Fine\"\n",
            "msgstr \"OK\"\n",
        );
        let map = parse_po(po);
        assert!(!map.contains_key("NoTranslationLineAtAll"), "{map:?}");
        assert_eq!(map.get("Fine").map(String::as_str), Some("OK"));
    }

    #[test]
    fn parse_po_never_panics_on_a_blank_or_garbage_file() {
        assert!(parse_po("").is_empty());
        assert!(parse_po("\n\n\n").is_empty());
        assert!(parse_po("not a po file at all").is_empty());
        assert!(parse_po("msgid").is_empty());
        assert!(parse_po("msgid \"unterminated").is_empty());
    }
}
