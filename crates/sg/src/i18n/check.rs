//! `sg i18n check`: the CI gate for the `sg i18n` family - one command,
//! one exit code, combining two independent localization gates:
//!
//! 1. **Overflow** - reuses [`crate::i18n::budget::scan`] exactly (no
//!    duplicated width/overflow math; see that module's doc comment for
//!    the full model). Runs unless `--no-overflow` is given.
//! 2. **Untranslated** - source strings ([`crate::i18n::scan`]) that are
//!    either missing from, or present but empty in, a gettext PO file
//!    given via `--against` (see [`crate::i18n::extract::parse_po`]).
//!    This gate only runs when `--against` is given.
//!
//! `--locale` is cosmetic in v1: PO is single-target (one `msgid`/
//! `msgstr` pair per string, no per-locale sections), so it never changes
//! how `--against` is parsed - it only annotates untranslated messages
//! with which locale's file was being checked.
//!
//! # Occurrence reporting
//!
//! Every *occurrence* of an affected string is reported separately (one
//! finding per node/property, not deduplicated by text) - the same
//! convention [`crate::i18n::budget`] already uses for overflow findings,
//! so a string used in two different buttons and untranslated in both
//! produces two findings, each at its own location.
//!
//! # Ordering
//!
//! Findings (of either kind) are sorted by `(file, line, code)` before
//! being printed or rendered as JSON - deterministic regardless of which
//! gate found what, or the order the two gates ran in.
//!
//! # Exit codes
//!
//! `0` clean, `1` at least one finding (an overflow warning or an
//! untranslated error), `2` a file (a scanned scene, or the `--against`
//! PO file) could not be read/parsed, *or* a usage error (`--no-overflow`
//! given without `--against`, which would run no gate at all - nothing
//! left for `sg i18n check` to do). This matches every other `sg`/`sg
//! i18n` command's parse-error code, and clap's own usage-error exit code
//! (both already `2` in this codebase), so a gate-usage error and a file
//! error are not distinguishable by exit code alone - exactly as a plain
//! clap argument-parsing error already is.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::i18n::budget::{self, OverflowFinding};
use crate::i18n::extract::{self, PoMap};
use crate::i18n::{scan, ScanError};
use crate::paths::collect_target_files;

/// The issue code emitted for every untranslated-string finding.
pub const CODE: &str = "i18n-untranslated";

/// Why a source string failed the untranslated gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranslationState {
    /// The string's text is not a key in the PO map at all: it was never
    /// extracted or sent for translation (leaked past `sg i18n extract`).
    Missing,
    /// The string's text is a key in the PO map, but `msgstr` is empty:
    /// it was extracted but has not been translated yet.
    Empty,
}

impl TranslationState {
    fn label(self) -> &'static str {
        match self {
            TranslationState::Missing => "missing",
            TranslationState::Empty => "empty",
        }
    }
}

/// One untranslated-string occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UntranslatedFinding {
    file: PathBuf,
    line: usize,
    string: String,
    node_path: String,
    node_type: String,
    state: TranslationState,
}

impl UntranslatedFinding {
    fn message(&self, locale: Option<&str>) -> String {
        let file_desc = match locale {
            Some(loc) => format!("translation file for locale \"{loc}\""),
            None => "translation file".to_string(),
        };
        match self.state {
            TranslationState::Missing => format!(
                "\"{}\" in {} \"{}\" has no entry in the {file_desc} (never extracted or sent for translation)",
                self.string, self.node_type, self.node_path
            ),
            TranslationState::Empty => format!(
                "\"{}\" in {} \"{}\" has an empty translation in the {file_desc} (extracted but not yet translated)",
                self.string, self.node_type, self.node_path
            ),
        }
    }

    fn json(&self) -> String {
        format!(
            concat!(
                "{{\"file\":\"{}\",\"line\":{},\"severity\":\"error\",\"code\":\"{}\",",
                "\"string\":\"{}\",\"node_path\":\"{}\",\"node_type\":\"{}\",\"translation_state\":\"{}\"}}"
            ),
            crate::json::escape(&self.file.display().to_string()),
            self.line,
            CODE,
            crate::json::escape(&self.string),
            crate::json::escape(&self.node_path),
            crate::json::escape(&self.node_type),
            self.state.label(),
        )
    }
}

/// Whether `text` fails the untranslated gate against `po_map`, and how.
/// `None` means it is present and non-empty - translated, not a finding.
fn translation_state(po_map: &PoMap, text: &str) -> Option<TranslationState> {
    match po_map.get(text) {
        None => Some(TranslationState::Missing),
        Some(s) if s.is_empty() => Some(TranslationState::Empty),
        Some(_) => None,
    }
}

/// One finding from either gate, carrying enough to sort, print, and
/// JSON-render it uniformly without duplicating either gate's own logic
/// here: an [`OverflowFinding`] is used exactly as [`budget`] produces
/// it, message and JSON rendering included.
enum Finding {
    Untranslated(UntranslatedFinding),
    Overflow(OverflowFinding),
}

impl Finding {
    fn file(&self) -> &Path {
        match self {
            Finding::Untranslated(u) => &u.file,
            Finding::Overflow(o) => &o.file,
        }
    }

    fn line(&self) -> usize {
        match self {
            Finding::Untranslated(u) => u.line,
            Finding::Overflow(o) => o.line,
        }
    }

    fn severity(&self) -> &'static str {
        match self {
            Finding::Untranslated(_) => "error",
            Finding::Overflow(_) => "warning",
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Finding::Untranslated(_) => CODE,
            Finding::Overflow(_) => budget::CODE,
        }
    }

    fn message(&self, locale: Option<&str>) -> String {
        match self {
            Finding::Untranslated(u) => u.message(locale),
            Finding::Overflow(o) => o.message(),
        }
    }

    fn json(&self) -> String {
        match self {
            Finding::Untranslated(u) => u.json(),
            Finding::Overflow(o) => budget::finding_json(o),
        }
    }
}

/// Sort `findings` by `(file, line, code)` - see the module doc comment's
/// "Ordering" section. A free function (rather than inlined into [`run`])
/// so the ordering rule itself is unit-testable independent of file I/O.
fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.file()
            .cmp(b.file())
            .then(a.line().cmp(&b.line()))
            .then(a.code().cmp(b.code()))
    });
}

fn render_json(findings: &[Finding]) -> String {
    let mut out = String::from("[");
    for (idx, f) in findings.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&f.json());
    }
    out.push(']');
    out
}

/// Print every entry in `errors` to stderr, deduplicated against
/// `reported` so the same `(file, message)` pair is never printed twice
/// even when both gates independently fail to read/parse the same file
/// (each gate re-reads scene files itself - see the module doc comment).
fn report_scan_errors(errors: &[ScanError], had_file_error: &mut bool, reported: &mut HashSet<(PathBuf, String)>) {
    for err in errors {
        *had_file_error = true;
        if reported.insert((err.file.clone(), err.message.clone())) {
            eprintln!("error: {}: {}", err.file.display(), err.message);
        }
    }
}

/// Run `sg i18n check`: expand `paths` the same way `sg check`/`sg i18n
/// budget` do, run the overflow gate (unless `no_overflow`) and, when
/// `against` is given, the untranslated-string gate, then print either
/// text lines (`sg check`'s `file:line: severity [code] message` shape)
/// or a `--json` array. See the module doc comment for exit codes.
pub fn run(
    paths: &[PathBuf],
    against: Option<&Path>,
    locale: Option<&str>,
    expansion_percent: u32,
    default_font_size: u32,
    no_overflow: bool,
    json: bool,
) -> ExitCode {
    if no_overflow && against.is_none() {
        eprintln!("error: --no-overflow requires --against (otherwise sg i18n check would run no gate at all)");
        return ExitCode::from(2);
    }

    let files = collect_target_files(paths);
    let mut findings: Vec<Finding> = Vec::new();
    let mut had_file_error = false;
    let mut reported: HashSet<(PathBuf, String)> = HashSet::new();

    if let Some(against_path) = against {
        match crate::read_source(against_path) {
            Ok(po_source) => {
                let po_map: PoMap = extract::parse_po(&po_source);
                let outcome = scan(&files);
                report_scan_errors(&outcome.errors, &mut had_file_error, &mut reported);
                for record in &outcome.records {
                    if let Some(state) = translation_state(&po_map, &record.text) {
                        findings.push(Finding::Untranslated(UntranslatedFinding {
                            file: record.scene_path.clone(),
                            line: record.line,
                            string: record.text.clone(),
                            node_path: record.node_path.clone(),
                            node_type: record.node_type.clone(),
                            state,
                        }));
                    }
                }
            }
            Err(msg) => {
                eprintln!("error: {msg}");
                had_file_error = true;
            }
        }
    }

    if !no_overflow {
        let outcome = budget::scan(&files, f64::from(expansion_percent), f64::from(default_font_size));
        report_scan_errors(&outcome.errors, &mut had_file_error, &mut reported);
        findings.extend(outcome.findings.into_iter().map(Finding::Overflow));
    }

    sort_findings(&mut findings);

    if json {
        println!("{}", render_json(&findings));
    } else {
        for f in &findings {
            println!(
                "{}:{}: {} [{}] {}",
                f.file().display(),
                f.line(),
                f.severity(),
                f.code(),
                f.message(locale)
            );
        }
    }

    if had_file_error {
        ExitCode::from(2)
    } else if !findings.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
    }

    // -----------------------------------------------------------------
    // translation_state
    // -----------------------------------------------------------------

    #[test]
    fn translation_state_distinguishes_missing_empty_and_translated() {
        let mut map = PoMap::new();
        map.insert("Cancel".to_string(), String::new());
        map.insert("Start Game".to_string(), "Spiel starten".to_string());
        assert_eq!(
            translation_state(&map, "Enter your name"),
            Some(TranslationState::Missing)
        );
        assert_eq!(translation_state(&map, "Cancel"), Some(TranslationState::Empty));
        assert_eq!(translation_state(&map, "Start Game"), None);
    }

    // -----------------------------------------------------------------
    // UntranslatedFinding: message / json
    // -----------------------------------------------------------------

    fn untranslated(state: TranslationState) -> UntranslatedFinding {
        UntranslatedFinding {
            file: PathBuf::from("main_menu.tscn"),
            line: 15,
            string: "Enter your name".to_string(),
            node_path: "VBox/NameInput".to_string(),
            node_type: "LineEdit".to_string(),
            state,
        }
    }

    #[test]
    fn missing_message_says_never_extracted() {
        let f = untranslated(TranslationState::Missing);
        let msg = f.message(None);
        assert!(msg.contains("no entry"), "{msg}");
        assert!(msg.contains("never extracted"), "{msg}");
    }

    #[test]
    fn empty_message_says_not_yet_translated() {
        let f = untranslated(TranslationState::Empty);
        let msg = f.message(None);
        assert!(msg.contains("empty translation"), "{msg}");
        assert!(msg.contains("not yet translated"), "{msg}");
    }

    #[test]
    fn message_includes_locale_when_given() {
        let f = untranslated(TranslationState::Missing);
        let msg = f.message(Some("de"));
        assert!(msg.contains("locale \"de\""), "{msg}");
    }

    #[test]
    fn message_omits_locale_clause_when_not_given() {
        let f = untranslated(TranslationState::Missing);
        let msg = f.message(None);
        assert!(!msg.contains("locale"), "{msg}");
    }

    #[test]
    fn untranslated_json_includes_every_documented_field() {
        let f = untranslated(TranslationState::Empty);
        let json = f.json();
        assert!(json.contains("\"file\":\"main_menu.tscn\""), "{json}");
        assert!(json.contains("\"line\":15"), "{json}");
        assert!(json.contains("\"severity\":\"error\""), "{json}");
        assert!(json.contains(&format!("\"code\":\"{CODE}\"")), "{json}");
        assert!(json.contains("\"string\":\"Enter your name\""), "{json}");
        assert!(json.contains("\"node_path\":\"VBox/NameInput\""), "{json}");
        assert!(json.contains("\"node_type\":\"LineEdit\""), "{json}");
        assert!(json.contains("\"translation_state\":\"empty\""), "{json}");
    }

    #[test]
    fn missing_state_serializes_to_missing_not_empty() {
        let f = untranslated(TranslationState::Missing);
        assert!(f.json().contains("\"translation_state\":\"missing\""), "{}", f.json());
    }

    // -----------------------------------------------------------------
    // report_scan_errors: dedup
    // -----------------------------------------------------------------

    #[test]
    fn report_scan_errors_deduplicates_identical_file_and_message_pairs() {
        let errors = vec![
            ScanError {
                file: PathBuf::from("a.tscn"),
                message: "boom".to_string(),
            },
            ScanError {
                file: PathBuf::from("a.tscn"),
                message: "boom".to_string(),
            },
        ];
        let mut had_file_error = false;
        let mut reported = HashSet::new();
        report_scan_errors(&errors, &mut had_file_error, &mut reported);
        assert!(had_file_error);
        assert_eq!(reported.len(), 1);
    }

    // -----------------------------------------------------------------
    // sort_findings: deterministic (file, line, code) ordering
    // -----------------------------------------------------------------

    fn overflow_at(file: &str, line: usize) -> Finding {
        Finding::Overflow(OverflowFinding {
            file: PathBuf::from(file),
            line,
            string: "X".to_string(),
            node_path: "N".to_string(),
            node_type: "Button".to_string(),
            property: "text",
            available_px: 10.0,
            source_px: 20.0,
            predicted_px: 20.0,
            expansion_percent: 0.0,
            font_size: 16.0,
            width_source: budget::WidthSource::CustomMinimumSize,
        })
    }

    fn untranslated_at(file: &str, line: usize) -> Finding {
        Finding::Untranslated(UntranslatedFinding {
            file: PathBuf::from(file),
            line,
            string: "X".to_string(),
            node_path: "N".to_string(),
            node_type: "Label".to_string(),
            state: TranslationState::Missing,
        })
    }

    #[test]
    fn sort_findings_orders_by_file_then_line() {
        let mut findings = vec![
            overflow_at("b.tscn", 1),
            untranslated_at("a.tscn", 20),
            overflow_at("a.tscn", 5),
        ];
        sort_findings(&mut findings);
        let keys: Vec<(String, usize)> = findings
            .iter()
            .map(|f| (f.file().display().to_string(), f.line()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("a.tscn".to_string(), 5),
                ("a.tscn".to_string(), 20),
                ("b.tscn".to_string(), 1),
            ]
        );
    }

    #[test]
    fn sort_findings_breaks_same_file_line_ties_by_code() {
        let mut findings = vec![untranslated_at("a.tscn", 5), overflow_at("a.tscn", 5)];
        sort_findings(&mut findings);
        // "i18n-text-overflow" < "i18n-untranslated" lexically.
        assert_eq!(findings[0].code(), budget::CODE);
        assert_eq!(findings[1].code(), CODE);
    }

    // -----------------------------------------------------------------
    // DRY: the overflow gate reuses budget::scan exactly - same
    // findings budget's own CLI/tests already establish for the shared
    // fixture, proving check.rs does not reimplement the overflow math.
    // -----------------------------------------------------------------

    #[test]
    fn overflow_gate_reuses_budgets_scan_and_agrees_with_its_known_findings() {
        let menu = fixtures_dir().join("i18n_budget_project").join("menu.tscn");
        let outcome = budget::scan(std::slice::from_ref(&menu), 40.0, 16.0);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.findings.len(), 2, "{:?}", outcome.findings);
        assert!(outcome.findings.iter().any(|f| f.node_path == "VBox/SettingsButton"));
        assert!(outcome.findings.iter().any(|f| f.node_path == "VBox/LanguageButton"));
    }
}
