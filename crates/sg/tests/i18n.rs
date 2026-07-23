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
