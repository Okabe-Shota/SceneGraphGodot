//! End-to-end tests for `sg i18n extract` against `fixtures/i18n_project/`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn i18n_fixture(name: &str) -> PathBuf {
    fixtures_dir().join("i18n_project").join(name)
}

fn budget_fixture(name: &str) -> PathBuf {
    fixtures_dir().join("i18n_budget_project").join(name)
}

fn sg() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sg"))
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn fresh_temp_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("sg-i18n-test-{label}-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

const EXPECTED_PO: &str = concat!(
    "msgid \"\"\n",
    "msgstr \"\"\n",
    "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
    "\"Content-Transfer-Encoding: 8bit\\n\"\n",
    "\n",
    "#. Type: Label | Screen: MainMenu | Property: text\n",
    "#: res://main_menu.tscn:VBox/TitleLabel\n",
    "msgid \"Welcome\"\n",
    "msgstr \"\"\n",
    "\n",
    "#. Type: Button | Screen: MainMenu | Property: text\n",
    "#: res://main_menu.tscn:VBox/StartButton\n",
    "msgid \"Start Game\"\n",
    "msgstr \"\"\n",
    "\n",
    "#. Type: Button | Screen: MainMenu | Property: tooltip_text\n",
    "#: res://main_menu.tscn:VBox/StartButton\n",
    "msgid \"Begin your adventure\"\n",
    "msgstr \"\"\n",
    "\n",
    "#. Type: LineEdit | Screen: MainMenu | Property: placeholder_text\n",
    "#: res://main_menu.tscn:VBox/NameInput\n",
    "msgid \"Enter your name\"\n",
    "msgstr \"\"\n",
    "\n",
    "#. Type: Button | Screen: MainMenu | Property: text\n",
    "#. Type: Button | Screen: MainMenu | Property: text\n",
    "#: res://main_menu.tscn:VBox/CancelButton\n",
    "#: res://main_menu.tscn:VBox/CloseButton\n",
    "msgid \"Cancel\"\n",
    "msgstr \"\"\n",
);

const EXPECTED_CSV: &str = concat!(
    "key,source,context\n",
    "Welcome,Welcome,Type: Label | Screen: MainMenu | Property: text | Ref: res://main_menu.tscn:VBox/TitleLabel\n",
    "Start Game,Start Game,Type: Button | Screen: MainMenu | Property: text | Ref: res://main_menu.tscn:VBox/StartButton\n",
    "Begin your adventure,Begin your adventure,Type: Button | Screen: MainMenu | Property: tooltip_text | Ref: res://main_menu.tscn:VBox/StartButton\n",
    "Enter your name,Enter your name,Type: LineEdit | Screen: MainMenu | Property: placeholder_text | Ref: res://main_menu.tscn:VBox/NameInput\n",
    "Cancel,Cancel,Type: Button | Screen: MainMenu | Property: text | Ref: res://main_menu.tscn:VBox/CancelButton\n",
    "Cancel,Cancel,Type: Button | Screen: MainMenu | Property: text | Ref: res://main_menu.tscn:VBox/CloseButton\n",
);

#[test]
fn extract_produces_expected_po_by_default() {
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg(i18n_fixture("main_menu.tscn"))
        .output()
        .expect("failed to run sg i18n extract");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, EXPECTED_PO);
}

#[test]
fn extract_format_csv_produces_expected_csv() {
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg(i18n_fixture("main_menu.tscn"))
        .arg("--format")
        .arg("csv")
        .output()
        .expect("failed to run sg i18n extract --format csv");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, EXPECTED_CSV);
}

#[test]
fn extract_output_flag_writes_to_a_file_instead_of_stdout() {
    let dir = fresh_temp_dir("output-flag");
    let out_file = dir.join("strings.po");

    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg(i18n_fixture("main_menu.tscn"))
        .arg("--output")
        .arg(&out_file)
        .output()
        .expect("failed to run sg i18n extract --output");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).is_empty(),
        "stdout must be empty when --output is given"
    );

    let written = fs::read_to_string(&out_file).unwrap();
    assert_eq!(written, EXPECTED_PO);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn extract_on_a_scene_with_no_translatable_strings_yields_just_the_po_header() {
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg(i18n_fixture("empty.tscn"))
        .output()
        .expect("failed to run sg i18n extract");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout,
        concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
            "\"Content-Transfer-Encoding: 8bit\\n\"\n",
        )
    );
}

#[test]
fn extract_on_a_scene_with_no_translatable_strings_yields_just_the_csv_header() {
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg(i18n_fixture("empty.tscn"))
        .arg("--format")
        .arg("csv")
        .output()
        .expect("failed to run sg i18n extract --format csv");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "key,source,context\n");
}

#[test]
fn extract_recurses_into_a_directory_argument() {
    // Passing the whole fixture project directory must find both .tscn
    // files (directories are searched recursively, same as `sg check`).
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg(fixtures_dir().join("i18n_project"))
        .output()
        .expect("failed to run sg i18n extract");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("msgid \"Start Game\""), "{stdout}");
    assert!(stdout.contains("msgid \"Welcome\""), "{stdout}");
}

#[test]
fn extract_with_no_paths_yields_just_the_header_and_exit_zero() {
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .output()
        .expect("failed to run sg i18n extract");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout,
        concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
            "\"Content-Transfer-Encoding: 8bit\\n\"\n",
        )
    );
}

#[test]
fn extract_reports_an_error_and_nonzero_exit_for_a_missing_file() {
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg(fixtures_dir().join("does_not_exist.tscn"))
        .output()
        .expect("failed to run sg i18n extract");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("error"));
}

#[test]
fn extract_help_documents_format_and_output_flags() {
    let output = sg()
        .arg("i18n")
        .arg("extract")
        .arg("--help")
        .output()
        .expect("failed to run sg i18n extract --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--format"), "{stdout}");
    assert!(stdout.contains("--output"), "{stdout}");
}

// -----------------------------------------------------------------------
// sg i18n budget
// -----------------------------------------------------------------------

fn run_budget(args: &[&str]) -> (i32, String, String) {
    let output = sg()
        .arg("i18n")
        .arg("budget")
        .args(args)
        .output()
        .expect("failed to run sg i18n budget");
    (
        output.status.code().expect("no exit code"),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

const EXPECTED_SETTINGS_MESSAGE: &str = "\"Settings\" in Button \"VBox/SettingsButton\" may overflow: predicted ~76px (source ~54px +40%) exceeds ~70px available (custom_minimum_size, font_size 16)";

#[test]
fn budget_flags_the_overflowing_fixed_width_button_and_exits_one() {
    let menu = budget_fixture("menu.tscn");
    let (code, stdout, stderr) = run_budget(&[menu.to_str().unwrap()]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("menu.tscn:9:"), "{stdout}");
    assert!(stdout.contains("warning [i18n-text-overflow]"), "{stdout}");
    assert!(stdout.contains(EXPECTED_SETTINGS_MESSAGE), "{stdout}");
}

#[test]
fn budget_does_not_flag_a_short_text_that_fits_its_fixed_width_button() {
    let menu = budget_fixture("menu.tscn");
    let (_, stdout, _) = run_budget(&[menu.to_str().unwrap()]);
    assert!(!stdout.contains("OkButton"), "{stdout}");
}

#[test]
fn budget_skips_a_label_with_autowrap_enabled() {
    let menu = budget_fixture("menu.tscn");
    let (_, stdout, _) = run_budget(&[menu.to_str().unwrap()]);
    assert!(!stdout.contains("HintLabel"), "{stdout}");
}

#[test]
fn budget_skips_a_control_whose_anchors_stretch_horizontally() {
    let menu = budget_fixture("menu.tscn");
    let (_, stdout, _) = run_budget(&[menu.to_str().unwrap()]);
    assert!(!stdout.contains("Banner"), "{stdout}");
}

#[test]
fn budget_flags_a_cjk_string_that_overflows_a_narrow_fixed_width_button() {
    let menu = budget_fixture("menu.tscn");
    let (_, stdout, _) = run_budget(&[menu.to_str().unwrap()]);
    assert!(stdout.contains("LanguageButton"), "{stdout}");
    assert!(stdout.contains("設定"), "{stdout}");
}

#[test]
fn budget_json_shape_includes_every_documented_field() {
    let menu = budget_fixture("menu.tscn");
    let (code, stdout, stderr) = run_budget(&[menu.to_str().unwrap(), "--json"]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.trim_start().starts_with('['), "{stdout}");
    assert!(stdout.trim_end().ends_with(']'), "{stdout}");
    assert!(stdout.contains("\"code\":\"i18n-text-overflow\""), "{stdout}");
    assert!(stdout.contains("\"severity\":\"warning\""), "{stdout}");
    assert!(stdout.contains("\"string\":\"Settings\""), "{stdout}");
    assert!(stdout.contains("\"node_path\":\"VBox/SettingsButton\""), "{stdout}");
    assert!(stdout.contains("\"node_type\":\"Button\""), "{stdout}");
    assert!(stdout.contains("\"property\":\"text\""), "{stdout}");
    assert!(stdout.contains("\"available_px\":70"), "{stdout}");
    assert!(stdout.contains("\"source_px\":54"), "{stdout}");
    assert!(stdout.contains("\"predicted_px\":76"), "{stdout}");
    assert!(stdout.contains("\"expansion_percent\":40"), "{stdout}");
    assert!(stdout.contains("\"font_size\":16"), "{stdout}");
    assert!(stdout.contains("\"width_source\":\"custom_minimum_size\""), "{stdout}");
}

#[test]
fn budget_expansion_zero_drops_the_boundary_warning_but_keeps_the_cjk_one() {
    let menu = budget_fixture("menu.tscn");
    let (code, stdout, _) = run_budget(&[menu.to_str().unwrap(), "--expansion", "0"]);
    // At 0% expansion, "Settings" (source ~54px) fits its 70px button and
    // must no longer warn...
    assert!(!stdout.contains("SettingsButton"), "{stdout}");
    // ...but the CJK button (source 32px vs a 30px available width) still
    // overflows even with no expansion applied at all.
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("LanguageButton"), "{stdout}");
}

#[test]
fn budget_high_expansion_flags_a_button_that_fits_by_default() {
    let menu = budget_fixture("menu.tscn");
    let (_, default_stdout, _) = run_budget(&[menu.to_str().unwrap()]);
    assert!(!default_stdout.contains("OkButton"), "{default_stdout}");

    let (code, stdout, _) = run_budget(&[menu.to_str().unwrap(), "--expansion", "500"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("OkButton"), "{stdout}");
}

#[test]
fn budget_default_font_size_flag_affects_results() {
    let menu = budget_fixture("menu.tscn");
    let (_, default_stdout, _) = run_budget(&[menu.to_str().unwrap()]);
    assert!(!default_stdout.contains("NoOverrideButton"), "{default_stdout}");

    let (code, stdout, _) = run_budget(&[menu.to_str().unwrap(), "--default-font-size", "24"]);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("NoOverrideButton"), "{stdout}");
}

#[test]
fn budget_scene_with_only_undeterminable_controls_yields_no_warnings_and_exit_zero() {
    let scene = budget_fixture("undeterminable.tscn");
    let (code, stdout, stderr) = run_budget(&[scene.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.trim().is_empty(), "{stdout}");
}

#[test]
fn budget_undeterminable_scene_json_is_an_empty_array() {
    let scene = budget_fixture("undeterminable.tscn");
    let (code, stdout, _) = run_budget(&[scene.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[]");
}

#[test]
fn budget_reports_an_error_and_exit_2_for_a_missing_file() {
    let (code, _stdout, stderr) = run_budget(&[fixtures_dir().join("does_not_exist.tscn").to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(stderr.contains("error"), "{stderr}");
}

#[test]
fn budget_help_documents_expansion_default_font_size_json_and_their_defaults() {
    let (code, stdout, _) = run_budget(&["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("--expansion"), "{stdout}");
    assert!(stdout.contains("--default-font-size"), "{stdout}");
    assert!(stdout.contains("--json"), "{stdout}");
    assert!(stdout.contains("40"), "{stdout}");
    assert!(stdout.contains("16"), "{stdout}");
}

// -----------------------------------------------------------------------
// sg i18n check
// -----------------------------------------------------------------------

fn run_check(args: &[&str]) -> (i32, String, String) {
    let output = sg()
        .arg("i18n")
        .arg("check")
        .args(args)
        .output()
        .expect("failed to run sg i18n check");
    (
        output.status.code().expect("no exit code"),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn de_po() -> PathBuf {
    i18n_fixture("translations.de.po")
}

#[test]
fn check_against_reports_exactly_the_missing_and_empty_occurrences() {
    let scene = i18n_fixture("main_menu.tscn");
    let (code, stdout, stderr) = run_check(&[scene.to_str().unwrap(), "--against", de_po().to_str().unwrap()]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");

    // "Enter your name" was never put in the PO file at all: missing.
    assert!(stdout.contains("main_menu.tscn:15:"), "{stdout}");
    assert!(stdout.contains("error [i18n-untranslated]"), "{stdout}");
    assert!(stdout.contains("\"Enter your name\""), "{stdout}");
    assert!(stdout.contains("VBox/NameInput"), "{stdout}");
    assert!(stdout.contains("no entry"), "{stdout}");
    assert!(stdout.contains("never extracted"), "{stdout}");

    // "Cancel" is in the PO file but msgstr is empty: reported once per
    // occurrence (CancelButton at line 21, CloseButton at line 24).
    assert!(stdout.contains("main_menu.tscn:21:"), "{stdout}");
    assert!(stdout.contains("VBox/CancelButton"), "{stdout}");
    assert!(stdout.contains("main_menu.tscn:24:"), "{stdout}");
    assert!(stdout.contains("VBox/CloseButton"), "{stdout}");
    assert_eq!(stdout.matches("empty translation").count(), 2, "{stdout}");

    // "Welcome", "Start Game", "Begin your adventure" are fully
    // translated - never mentioned.
    assert!(!stdout.contains("\"Welcome\""), "{stdout}");
    assert!(!stdout.contains("\"Start Game\""), "{stdout}");
    assert!(!stdout.contains("\"Begin your adventure\""), "{stdout}");

    // main_menu.tscn sets no control geometry at all, so the (default,
    // still-enabled) overflow gate finds nothing.
    assert!(!stdout.contains("i18n-text-overflow"), "{stdout}");

    // Exactly three findings total.
    assert_eq!(stdout.lines().count(), 3, "{stdout}");
}

#[test]
fn check_without_against_runs_the_overflow_gate_only() {
    let menu = budget_fixture("menu.tscn");
    let (code, stdout, stderr) = run_check(&[menu.to_str().unwrap()]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("warning [i18n-text-overflow]"), "{stdout}");
    assert!(stdout.contains(EXPECTED_SETTINGS_MESSAGE), "{stdout}");
    assert!(!stdout.contains("i18n-untranslated"), "{stdout}");
}

#[test]
fn check_no_overflow_without_against_is_a_usage_error() {
    let scene = i18n_fixture("main_menu.tscn");
    let (code, stdout, stderr) = run_check(&[scene.to_str().unwrap(), "--no-overflow"]);
    assert_eq!(code, 2, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("--no-overflow"), "{stderr}");
    assert!(stderr.contains("--against"), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
}

#[test]
fn check_no_overflow_with_against_runs_untranslated_gate_only() {
    // The budget fixture's menu.tscn has real overflow risk (see
    // fixtures/i18n_budget_project/menu.tscn / sg i18n budget's own
    // tests), but none of its strings are in translations.de.po at all -
    // --no-overflow must suppress every i18n-text-overflow line while
    // still reporting the (many) i18n-untranslated ones.
    let menu = budget_fixture("menu.tscn");
    let (code, stdout, stderr) = run_check(&[
        menu.to_str().unwrap(),
        "--against",
        de_po().to_str().unwrap(),
        "--no-overflow",
    ]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(!stdout.contains("i18n-text-overflow"), "{stdout}");
    assert!(stdout.contains("i18n-untranslated"), "{stdout}");
    assert!(stdout.contains("\"Settings\""), "{stdout}");
}

#[test]
fn check_combined_run_reports_both_codes_in_deterministic_file_order() {
    let budget_menu = budget_fixture("menu.tscn");
    let project_menu = i18n_fixture("main_menu.tscn");
    let (code, stdout, stderr) = run_check(&[
        budget_menu.to_str().unwrap(),
        project_menu.to_str().unwrap(),
        "--against",
        de_po().to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("i18n-text-overflow"), "{stdout}");
    assert!(stdout.contains("i18n-untranslated"), "{stdout}");

    // Deterministic ordering: by file path, so every
    // i18n_budget_project/menu.tscn line precedes every
    // i18n_project/main_menu.tscn line ("i18n_budget_project" <
    // "i18n_project" lexically).
    let last_budget_line = stdout
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("i18n_budget_project"))
        .map(|(i, _)| i)
        .last();
    let first_project_line = stdout
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("i18n_project"))
        .map(|(i, _)| i)
        .next();
    if let (Some(last_b), Some(first_p)) = (last_budget_line, first_project_line) {
        assert!(last_b < first_p, "{stdout}");
    }
}

#[test]
fn check_json_shape_for_untranslated_findings() {
    let scene = i18n_fixture("main_menu.tscn");
    let (code, stdout, stderr) = run_check(&[
        scene.to_str().unwrap(),
        "--against",
        de_po().to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.trim_start().starts_with('['), "{stdout}");
    assert!(stdout.trim_end().ends_with(']'), "{stdout}");
    assert!(stdout.contains("\"code\":\"i18n-untranslated\""), "{stdout}");
    assert!(stdout.contains("\"severity\":\"error\""), "{stdout}");
    assert!(stdout.contains("\"translation_state\":\"missing\""), "{stdout}");
    assert!(stdout.contains("\"translation_state\":\"empty\""), "{stdout}");
    assert!(stdout.contains("\"node_path\":\"VBox/NameInput\""), "{stdout}");
    assert!(stdout.contains("\"node_type\":\"LineEdit\""), "{stdout}");
}

#[test]
fn check_json_shape_for_overflow_findings_matches_budgets_shape() {
    let menu = budget_fixture("menu.tscn");
    let (code, stdout, stderr) = run_check(&[menu.to_str().unwrap(), "--json"]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("\"code\":\"i18n-text-overflow\""), "{stdout}");
    assert!(stdout.contains("\"severity\":\"warning\""), "{stdout}");
    assert!(stdout.contains("\"width_source\":\"custom_minimum_size\""), "{stdout}");
    assert!(stdout.contains("\"available_px\":70"), "{stdout}");
}

#[test]
fn check_locale_flag_is_included_in_untranslated_messages() {
    let scene = i18n_fixture("main_menu.tscn");
    let (code, stdout, stderr) = run_check(&[
        scene.to_str().unwrap(),
        "--against",
        de_po().to_str().unwrap(),
        "--locale",
        "de",
    ]);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("locale \"de\""), "{stdout}");
}

#[test]
fn check_fully_translated_and_no_overflow_scene_exits_zero() {
    let dir = fresh_temp_dir("check-clean");
    let po_path = dir.join("full.po");
    fs::write(
        &po_path,
        concat!(
            "msgid \"\"\n",
            "msgstr \"\"\n",
            "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
            "\n",
            "msgid \"Welcome\"\n",
            "msgstr \"Willkommen\"\n",
            "\n",
            "msgid \"Start Game\"\n",
            "msgstr \"Spiel starten\"\n",
            "\n",
            "msgid \"Begin your adventure\"\n",
            "msgstr \"Beginne dein Abenteuer\"\n",
            "\n",
            "msgid \"Enter your name\"\n",
            "msgstr \"Gib deinen Namen ein\"\n",
            "\n",
            "msgid \"Cancel\"\n",
            "msgstr \"Abbrechen\"\n",
        ),
    )
    .unwrap();

    let scene = i18n_fixture("main_menu.tscn");
    let (code, stdout, stderr) = run_check(&[scene.to_str().unwrap(), "--against", po_path.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.trim().is_empty(), "{stdout}");

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_reports_an_error_and_exit_2_for_an_unreadable_against_file() {
    let scene = i18n_fixture("main_menu.tscn");
    let missing_po = fixtures_dir().join("does_not_exist.po");
    let (code, _stdout, stderr) = run_check(&[scene.to_str().unwrap(), "--against", missing_po.to_str().unwrap()]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("error"), "{stderr}");
}

#[test]
fn check_reports_an_error_and_exit_2_for_a_missing_scene_file() {
    let (code, _stdout, stderr) = run_check(&[fixtures_dir().join("does_not_exist.tscn").to_str().unwrap()]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("error"), "{stderr}");
}

#[test]
fn check_help_documents_all_flags_and_their_defaults() {
    let (code, stdout, _) = run_check(&["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("--against"), "{stdout}");
    assert!(stdout.contains("--locale"), "{stdout}");
    assert!(stdout.contains("--expansion"), "{stdout}");
    assert!(stdout.contains("--default-font-size"), "{stdout}");
    assert!(stdout.contains("--no-overflow"), "{stdout}");
    assert!(stdout.contains("--json"), "{stdout}");
    assert!(stdout.contains("40"), "{stdout}");
    assert!(stdout.contains("16"), "{stdout}");
}
