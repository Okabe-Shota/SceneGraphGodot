//! End-to-end tests for `sg check` and `sg fix` against the fixture
//! corpus: one detection test per rule, one fix test verifying either an
//! exact expected byte-for-byte output or the relevant invariants
//! (unused ids gone, referenced ids intact, load_steps recomputed),
//! idempotency, `--dry-run` never writing, and the required
//! check -> fix -> check demonstration on the composite fixture.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn broken_fixture(name: &str) -> PathBuf {
    fixtures_dir().join("broken").join(name)
}

fn sg() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sg"))
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Copies a broken fixture into a fresh temp directory so `sg fix` can
/// mutate it without touching the checked-in fixture. Never reuses a
/// directory across tests, even ones running in parallel.
fn copy_to_temp(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("sg-check-fix-test-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let dst = dir.join(name);
    fs::copy(broken_fixture(name), &dst).unwrap();
    dst
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn run_check_json(path: &Path) -> (i32, String) {
    let output = sg()
        .arg("check")
        .arg(path)
        .arg("--json")
        .output()
        .expect("failed to run sg check");
    (
        output.status.code().expect("no exit code"),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    )
}

fn run_fix(path: &Path, extra_args: &[&str]) -> (i32, String, String) {
    let output = sg()
        .arg("fix")
        .arg(path)
        .args(extra_args)
        .output()
        .expect("failed to run sg fix");
    (
        output.status.code().expect("no exit code"),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

// ---------------------------------------------------------------------
// Detection (sg check)
// ---------------------------------------------------------------------

#[test]
fn detects_load_steps_mismatch() {
    let (code, json) = run_check_json(&broken_fixture("01_load_steps_mismatch.tscn"));
    assert_eq!(code, 1);
    assert!(json.contains("\"code\":\"load-steps-mismatch\""), "{json}");
    assert!(json.contains("\"fixable\":true"), "{json}");
}

#[test]
fn detects_broken_reference_as_unfixable_error() {
    let (code, json) = run_check_json(&broken_fixture("02_broken_reference.tscn"));
    assert_eq!(code, 1);
    assert!(json.contains("\"code\":\"broken-sub-resource-ref\""), "{json}");
    assert!(json.contains("\"severity\":\"error\""), "{json}");
    assert!(json.contains("\"fixable\":false"), "{json}");
}

#[test]
fn detects_sub_resource_forward_reference() {
    let (code, json) = run_check_json(&broken_fixture("03_forward_reference.tscn"));
    assert_eq!(code, 1);
    assert!(json.contains("\"code\":\"sub-resource-forward-reference\""), "{json}");
    assert!(json.contains("\"fixable\":true"), "{json}");
}

#[test]
fn detects_child_before_parent() {
    let (code, json) = run_check_json(&broken_fixture("04_child_before_parent.tscn"));
    assert_eq!(code, 1);
    assert!(json.contains("\"code\":\"child-before-parent\""), "{json}");
    assert!(json.contains("\"fixable\":true"), "{json}");
}

#[test]
fn detects_unused_resource_chain_but_not_the_used_sibling() {
    let (code, json) = run_check_json(&broken_fixture("05_unused_resource_chain.tscn"));
    assert_eq!(code, 1);
    assert!(json.contains("\"code\":\"unused-ext-resource\""), "{json}");
    assert!(json.matches("\"code\":\"unused-sub-resource\"").count() == 2, "{json}");
    assert!(json.contains("shape_a"), "{json}");
    assert!(json.contains("shape_b"), "{json}");
    // shape_c is used (directly referenced by the node) and must never be
    // reported, even though a naive "is this id mentioned anywhere in the
    // file" scan would not distinguish it from shape_a/shape_b.
    assert!(!json.contains("shape_c"), "{json}");
}

#[test]
fn detects_duplicate_id_as_unfixable_error() {
    let (code, json) = run_check_json(&broken_fixture("06_duplicate_id.tscn"));
    assert_eq!(code, 1);
    assert!(json.contains("\"code\":\"duplicate-ext-resource-id\""), "{json}");
    assert!(json.contains("\"fixable\":false"), "{json}");
}

#[test]
fn detects_circular_sub_resource_reference_for_both_participants() {
    let (code, json) = run_check_json(&broken_fixture("07_circular_reference.tscn"));
    assert_eq!(code, 1);
    assert_eq!(
        json.matches("\"code\":\"circular-sub-resource-reference\"").count(),
        2,
        "{json}"
    );
    assert!(json.contains("\"fixable\":false"), "{json}");
    // A cycle must never also be reported as a plain forward reference.
    assert!(!json.contains("sub-resource-forward-reference"), "{json}");
}

#[test]
fn composite_fixture_has_only_fixable_warnings() {
    let (code, json) = run_check_json(&broken_fixture("08_composite.tscn"));
    assert_eq!(code, 1);
    for expected_code in [
        "load-steps-mismatch",
        "unused-ext-resource",
        "sub-resource-forward-reference",
        "child-before-parent",
    ] {
        assert!(
            json.contains(&format!("\"code\":\"{expected_code}\"")),
            "missing {expected_code}: {json}"
        );
    }
    assert!(!json.contains("\"severity\":\"error\""), "{json}");
}

#[test]
fn detects_broken_connection_node_path_as_unfixable_error() {
    let (code, json) = run_check_json(&broken_fixture("09_broken_connection_node_path.tscn"));
    assert_eq!(code, 1, "{json}");
    assert!(json.contains("\"code\":\"broken-connection-node-path\""), "{json}");
    assert!(json.contains("\"severity\":\"error\""), "{json}");
    assert!(json.contains("\"fixable\":false"), "{json}");
    assert!(json.contains("from=\\\"Buttn\\\""), "{json}");
}

#[test]
fn detects_duplicate_node_name_as_unfixable_error() {
    let (code, json) = run_check_json(&broken_fixture("10_duplicate_node_name.tscn"));
    assert_eq!(code, 1, "{json}");
    assert!(json.contains("\"code\":\"duplicate-node-name\""), "{json}");
    assert!(json.contains("\"severity\":\"error\""), "{json}");
    assert!(json.contains("\"fixable\":false"), "{json}");
    assert!(json.contains("\\\"Button\\\""), "{json}");
}

#[test]
fn fix_does_not_touch_or_panic_on_duplicate_node_name() {
    let path = broken_fixture("10_duplicate_node_name.tscn");
    let original = read(&path);
    let temp = copy_to_temp("10_duplicate_node_name.tscn");
    let (code, _out, _err) = run_fix(&temp, &[]);
    assert_eq!(code, 1);
    assert_eq!(read(&temp), original, "unfixable file must not be modified at all");

    let (check_code, json) = run_check_json(&temp);
    assert_eq!(check_code, 1);
    assert!(json.contains("duplicate-node-name"), "{json}");
}

#[test]
fn connection_into_instanced_child_scene_stays_clean() {
    // "Enemies" is an instanced sub-scene; "Enemies/Slime" is declared only
    // inside that sub-scene, which this file cannot see - must not be
    // reported as a broken connection target.
    let path = fixtures_dir().join("12_connection_into_instanced_child.tscn");
    let (code, json) = run_check_json(&path);
    assert_eq!(code, 0, "{json}");
    assert_eq!(json, "[]");
}

#[test]
fn fix_does_not_touch_or_panic_on_broken_connection_node_path() {
    let path = broken_fixture("09_broken_connection_node_path.tscn");
    let original = read(&path);
    let temp = copy_to_temp("09_broken_connection_node_path.tscn");
    let (code, _out, _err) = run_fix(&temp, &[]);
    assert_eq!(code, 1);
    assert_eq!(read(&temp), original, "unfixable file must not be modified at all");

    let (check_code, json) = run_check_json(&temp);
    assert_eq!(check_code, 1);
    assert!(json.contains("broken-connection-node-path"), "{json}");
}

// ---------------------------------------------------------------------
// ext_resource path existence / case on disk (no --engine involved)
// ---------------------------------------------------------------------

#[test]
fn detects_missing_ext_resource_path_without_engine() {
    // No `--engine` flag here at all: this is the static rule closing the
    // gap the README used to describe as engine-only.
    let path = fixtures_dir().join("engine_project").join("broken.tscn");
    let (code, json) = run_check_json(&path);
    assert_eq!(code, 1, "{json}");
    assert!(json.contains("\"code\":\"missing-ext-resource-path\""), "{json}");
    assert!(json.contains("\"severity\":\"error\""), "{json}");
    assert!(json.contains("\"fixable\":false"), "{json}");
    assert!(json.contains("res://scripts/does_not_exist.gd"), "{json}");
}

#[test]
fn valid_ext_resource_path_stays_clean_without_engine() {
    let path = fixtures_dir().join("engine_project").join("valid.tscn");
    let (code, json) = run_check_json(&path);
    assert_eq!(code, 0, "{json}");
    assert_eq!(json, "[]");
}

#[test]
fn detects_ext_resource_path_case_mismatch() {
    let path = fixtures_dir().join("case_mismatch_project").join("scene.tscn");
    let (code, json) = run_check_json(&path);
    assert_eq!(code, 1, "{json}");
    assert!(json.contains("\"code\":\"ext-resource-path-case-mismatch\""), "{json}");
    assert!(json.contains("\"severity\":\"warning\""), "{json}");
    assert!(json.contains("\"fixable\":false"), "{json}");
    // The path as written, and the actual on-disk casing, must both
    // appear in the message.
    assert!(json.contains("res://scripts/player.gd"), "{json}");
    assert!(json.contains("res://scripts/Player.gd"), "{json}");
}

#[test]
fn files_without_a_project_root_are_silently_skipped_for_path_checks() {
    // fixtures/broken/08_composite.tscn declares ext_resource sections
    // whose res:// paths don't exist anywhere on disk, but the fixture
    // has no project.godot ancestor - a res:// path is meaningless
    // without one, so the new rules must never fire here (that case is
    // `sg check --engine`'s `engine-project-not-found` territory, not a
    // new issue kind).
    let (_code, json) = run_check_json(&broken_fixture("08_composite.tscn"));
    assert!(!json.contains("missing-ext-resource-path"), "{json}");
    assert!(!json.contains("ext-resource-path-case-mismatch"), "{json}");
}

#[test]
fn fix_does_not_touch_or_panic_on_missing_ext_resource_path() {
    // Not fixable: `sg fix` must leave the file untouched and still
    // report the issue afterward, exactly like other unfixable rules.
    let dir = std::env::temp_dir().join(format!(
        "sg-check-fix-test-missing-path-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("project.godot"), "").unwrap();
    let scene = dir.join("scene.tscn");
    fs::write(
        &scene,
        concat!(
            "[gd_scene load_steps=2 format=3]\n",
            "\n",
            "[ext_resource type=\"Script\" path=\"res://missing.gd\" id=\"1_missing\"]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "script = ExtResource(\"1_missing\")\n",
        ),
    )
    .unwrap();
    let original = read(&scene);

    let (code, _out, _err) = run_fix(&scene, &[]);
    assert_eq!(code, 1);
    assert_eq!(read(&scene), original, "unfixable file must not be modified at all");

    let (check_code, json) = run_check_json(&scene);
    assert_eq!(check_code, 1);
    assert!(json.contains("missing-ext-resource-path"), "{json}");

    fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------
// Fixing (sg fix): exact byte-for-byte output where hand-derived
// ---------------------------------------------------------------------

#[test]
fn fixes_load_steps_with_minimal_diff() {
    let path = copy_to_temp("01_load_steps_mismatch.tscn");
    let original = read(&path);
    let (code, _out, _err) = run_fix(&path, &[]);
    assert_eq!(code, 0);
    let fixed = read(&path);
    let expected = original.replace("load_steps=99", "load_steps=2");
    assert_eq!(fixed, expected);

    // Idempotent: a second fix run changes nothing further.
    let (code2, _, _) = run_fix(&path, &[]);
    assert_eq!(code2, 0);
    assert_eq!(read(&path), fixed);

    let (check_code, _) = run_check_json(&path);
    assert_eq!(check_code, 0);
}

#[test]
fn fixes_forward_reference_by_reordering_sub_resources_only() {
    let path = copy_to_temp("03_forward_reference.tscn");
    let (code, _, _) = run_fix(&path, &[]);
    assert_eq!(code, 0);
    let expected = concat!(
        "[gd_scene load_steps=3 format=3]\n",
        "\n",
        "[sub_resource type=\"Shader\" id=\"shader_1\"]\n",
        "code = \"shader code\"\n",
        "\n",
        "[sub_resource type=\"ShaderMaterial\" id=\"mat_1\"]\n",
        "shader_param/detail = SubResource(\"shader_1\")\n",
        "\n",
        "[node name=\"Main\" type=\"Node2D\"]\n",
        "material = SubResource(\"mat_1\")\n",
    );
    assert_eq!(read(&path), expected);

    let (code2, _, _) = run_fix(&path, &[]);
    assert_eq!(code2, 0);
    assert_eq!(read(&path), expected);
}

#[test]
fn fixes_child_before_parent_by_reordering_node_sections_only() {
    let path = copy_to_temp("04_child_before_parent.tscn");
    let (code, _, _) = run_fix(&path, &[]);
    assert_eq!(code, 0);
    let expected = concat!(
        "[gd_scene load_steps=1 format=3]\n",
        "\n",
        "[node name=\"Main\" type=\"Node2D\"]\n",
        "\n",
        "[node name=\"Child\" type=\"Node2D\" parent=\".\"]\n",
        "\n",
    );
    assert_eq!(read(&path), expected);

    let (code2, _, _) = run_fix(&path, &[]);
    assert_eq!(code2, 0);
    assert_eq!(read(&path), expected);
}

#[test]
fn fixes_unused_resource_chain_and_recomputes_load_steps() {
    let path = copy_to_temp("05_unused_resource_chain.tscn");
    let (code, _, _) = run_fix(&path, &[]);
    assert_eq!(code, 0);
    let fixed = read(&path);

    assert!(!fixed.contains("1_unused"), "{fixed}");
    assert!(!fixed.contains("shape_a"), "{fixed}");
    assert!(!fixed.contains("shape_b"), "{fixed}");
    assert!(fixed.contains("2_used"), "{fixed}");
    assert!(fixed.contains("shape_c"), "{fixed}");
    // 1 ext_resource (2_used) + 1 sub_resource (shape_c) + 1.
    assert!(fixed.contains("load_steps=3"), "{fixed}");

    let (check_code, _) = run_check_json(&path);
    assert_eq!(check_code, 0);

    let (code2, _, _) = run_fix(&path, &[]);
    assert_eq!(code2, 0);
    assert_eq!(read(&path), fixed, "second fix run must be a no-op");
}

#[test]
fn keep_unused_flag_preserves_unused_resources() {
    let path = copy_to_temp("05_unused_resource_chain.tscn");
    let (code, _, _) = run_fix(&path, &["--keep-unused"]);
    let fixed = read(&path);

    // shape_a (unused, kept) itself forward-references shape_b (declared
    // right after it) - that is a genuine, independent rule-3 issue and
    // must still be fixed even with deletion disabled, so all five ids
    // survive but load_steps (unaffected, since nothing was deleted)
    // stays at its original, already-correct value of 6.
    for id in ["1_unused", "2_used", "shape_a", "shape_b", "shape_c"] {
        assert!(fixed.contains(id), "{id} missing from:\n{fixed}");
    }
    assert!(fixed.contains("load_steps=6"), "{fixed}");

    // The unused resources are still real, still-fixable issues (the
    // user simply declined to remove them this run), so `sg check` still
    // reports them and the exit code still reflects "not clean" - but the
    // forward reference between shape_a/shape_b must be gone.
    assert_eq!(code, 1);
    let (check_code, json) = run_check_json(&path);
    assert_eq!(check_code, 1);
    assert!(json.contains("unused-ext-resource"));
    assert!(json.contains("\"fixable\":true"));
    assert!(!json.contains("sub-resource-forward-reference"), "{json}");
}

#[test]
fn leaves_broken_reference_untouched_and_reports_it() {
    let path = broken_fixture("02_broken_reference.tscn");
    let original = read(&path);
    let temp = copy_to_temp("02_broken_reference.tscn");
    let (code, _out, _err) = run_fix(&temp, &[]);
    assert_eq!(code, 1);
    assert_eq!(read(&temp), original, "unfixable file must not be modified at all");
}

#[test]
fn leaves_duplicate_id_untouched_and_reports_it() {
    let path = copy_to_temp("06_duplicate_id.tscn");
    let original = read(&path);
    let (code, _out, _err) = run_fix(&path, &[]);
    assert_eq!(code, 1);
    assert_eq!(read(&path), original);

    let (check_code, json) = run_check_json(&path);
    assert_eq!(check_code, 1);
    assert!(json.contains("duplicate-ext-resource-id"));
}

#[test]
fn leaves_circular_reference_untouched_and_does_not_hang() {
    // The real assertion here is that this test completes at all: a
    // buggy reorder implementation that infinite-loops on a cycle would
    // hang the whole test binary.
    let path = copy_to_temp("07_circular_reference.tscn");
    let original = read(&path);
    let (code, _out, _err) = run_fix(&path, &[]);
    assert_eq!(code, 1);
    assert_eq!(read(&path), original);
}

#[test]
fn dry_run_never_writes_to_disk() {
    let path = copy_to_temp("01_load_steps_mismatch.tscn");
    let original = read(&path);
    let (code, stdout, _err) = run_fix(&path, &["--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(read(&path), original, "--dry-run must never modify the file");
    assert!(stdout.contains("@@"), "expected a unified diff hunk: {stdout}");
    assert!(stdout.contains("load_steps"), "{stdout}");
}

#[test]
fn dry_run_on_unfixable_file_reports_no_change_and_exit_one() {
    let path = copy_to_temp("07_circular_reference.tscn");
    let original = read(&path);
    let (code, _stdout, _err) = run_fix(&path, &["--dry-run"]);
    assert_eq!(code, 1);
    assert_eq!(read(&path), original);
}

// ---------------------------------------------------------------------
// The required composite demonstration: before has issues, after is
// clean, and a second fix pass is a true no-op.
// ---------------------------------------------------------------------

#[test]
fn composite_check_fix_check_demonstration() {
    let path = copy_to_temp("08_composite.tscn");

    let (before_code, before_json) = run_check_json(&path);
    assert_eq!(before_code, 1, "expected issues before fixing: {before_json}");
    assert!(!before_json.is_empty() && before_json != "[]");

    let (fix_code, _out, _err) = run_fix(&path, &[]);
    assert_eq!(fix_code, 0, "fix should resolve every issue in the composite fixture");

    let (after_code, after_json) = run_check_json(&path);
    assert_eq!(after_code, 0, "expected a clean file after fixing: {after_json}");
    assert_eq!(after_json, "[]");

    let fixed_once = read(&path);
    let (fix_code2, _, _) = run_fix(&path, &[]);
    assert_eq!(fix_code2, 0);
    assert_eq!(read(&path), fixed_once, "second fix run must be a no-op");
}

// ---------------------------------------------------------------------
// The existing well-formed fixture corpus must already be clean.
// ---------------------------------------------------------------------

#[test]
fn every_well_formed_fixture_is_check_clean_and_fix_dry_run_is_a_no_op() {
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
        let before = fs::read(&path).unwrap();
        let (check_code, json) = run_check_json(&path);
        assert_eq!(check_code, 0, "{}: expected clean, got {json}", path.display());

        let (fix_code, stdout, _err) = run_fix(&path, &["--dry-run"]);
        assert_eq!(fix_code, 0, "{}: dry-run should report clean", path.display());
        assert!(stdout.contains("clean"), "{}: {stdout}", path.display());

        let after = fs::read(&path).unwrap();
        assert_eq!(before, after, "{}: --dry-run must never touch the file", path.display());
        checked += 1;
    }
    assert!(
        checked >= 11,
        "expected at least 11 well-formed fixtures, found {checked}"
    );
}
