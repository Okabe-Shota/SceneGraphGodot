//! `sg i18n extract`: scans scene files for the v1 translatable-string set
//! ([`crate::i18n::TRANSLATABLE_PROPERTIES`]) via the shared scan layer in
//! [`crate::i18n`], and formats the result as a gettext PO file (default)
//! or a translator-facing CSV.

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
/// `res://ui/main_menu.tscn:VBox/StartButton`.
fn reference(record: &TranslatableString) -> String {
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
}
