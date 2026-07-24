//! `sg i18n shots`: the translator-facing deliverable of the `sg i18n`
//! family - a single self-contained HTML file ("translators.html") with
//! one row per translatable string occurrence, full context (scene, node
//! path, node type, screen, property, a `res://`-or-file reference), and,
//! opt-in and best-effort, a screenshot of the scene it came from.
//!
//! # Two independent halves
//!
//! 1. **Context (always on).** Reuses [`crate::i18n::scan`] exactly - the
//!    same walk `sg i18n extract`/`sg i18n check` already use - so this
//!    command never re-derives what a translatable string or a node's path
//!    is. This half needs no engine, no display, no GPU: it is a pure
//!    function of the parsed scene files, always reliable, and is the
//!    acceptance bar for this command.
//! 2. **Screenshots (opt-in via `--screenshots`, best-effort).** Attempts
//!    to render one frame of each scene through a generated GDScript, run
//!    via the exact same [`crate::engine::run_with_timeout`]/
//!    [`crate::engine::find_godot_binary`]/[`crate::engine::group_by_project`]
//!    machinery `sg check --engine` already uses - no duplicated process
//!    handling. **Godot's `--headless` flag uses a dummy rendering driver
//!    and cannot actually produce an image** - a real screenshot needs a
//!    display/GPU context that is not guaranteed to exist in CI or on a
//!    headless server. This is a known, documented constraint, not a bug
//!    to chase: every way this can fail (no binary, a scene that fails to
//!    load, a headless environment with no renderer, a timeout, a capture
//!    error) degrades to a per-scene "not captured: <reason>" note in the
//!    output HTML instead of failing the command. `sg i18n shots` always
//!    exits `0` on a successful scan regardless of whether any screenshot
//!    was actually captured - "not captured" is an expected, reported
//!    outcome, never a command failure. See README.md, "sg i18n shots",
//!    for the honest constraint spelled out for a reader who has not read
//!    this module.
//!
//! # HTML shape
//!
//! One `<section>` per scene (heading: screen name + reference path),
//! containing an optional screenshot block (only present at all when
//! `--screenshots` was given) followed by a table of every string found in
//! that scene: source text, node path, node type, property, and reference
//! (`res_path`-or-`scene_path` + `:node_path`, via
//! [`crate::i18n::extract::reference`] - the exact same computation `sg
//! i18n extract`'s PO/CSV output already uses, so a translator cross-
//! referencing this page against an extracted PO file sees the same
//! reference string in both places). Scenes are emitted in `(scene_path,
//! line)` order - deterministic regardless of scan order, so output is
//! byte-identical across runs over unchanged input and diffs cleanly.
//!
//! No external resources of any kind: CSS is inlined in a `<style>` block,
//! and a captured screenshot (if any) is embedded as a `data:` URI PNG via
//! a small hand-rolled base64 encoder ([`base64_encode`]) - the file this
//! command produces is meant to be opened offline, in any browser, by a
//! translator who has nothing else installed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::engine;
use crate::i18n::extract::reference;
use crate::i18n::{scan, TranslatableString};
use crate::paths::collect_target_files;

// ---------------------------------------------------------------------
// HTML escaping
// ---------------------------------------------------------------------

/// Escape `&`, `<`, `>`, `"`, and `'` for safe inclusion as HTML text
/// content or inside a double-quoted attribute value. This is the only
/// thing standing between a translator's source string (arbitrary,
/// untrusted-as-markup content pulled straight out of a `.tscn` file) and
/// a broken or spoofed page, so every string, node path, file path, and
/// reference rendered anywhere in the output goes through this (or
/// [`escape_html_multiline`]) - never interpolated raw.
pub(crate) fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// [`escape_html`], then turn embedded newlines into `<br>` so multi-line
/// source text (e.g. a multi-line `dialog_text`) renders as a visible line
/// break instead of collapsing into a single line the way a raw newline
/// would in HTML. Applied only to the source-string cell - every other
/// field this module renders (node path, node type, property, file/`res://`
/// reference) is never expected to contain a newline, so it goes through
/// plain [`escape_html`] instead.
pub(crate) fn escape_html_multiline(s: &str) -> String {
    escape_html(s).replace('\n', "<br>\n")
}

// ---------------------------------------------------------------------
// Base64 (hand-rolled - no new dependency for embedding a captured PNG)
// ---------------------------------------------------------------------

const BASE64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard (RFC 4648) base64 encoding with `=` padding. Small and
/// dependency-free by design - see the module doc comment and the
/// project's "no new dependencies" constraint.
pub(crate) fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let c0 = BASE64_ALPHABET[((n >> 18) & 0x3F) as usize];
        let c1 = BASE64_ALPHABET[((n >> 12) & 0x3F) as usize];
        let c2 = BASE64_ALPHABET[((n >> 6) & 0x3F) as usize];
        let c3 = BASE64_ALPHABET[(n & 0x3F) as usize];
        out.push(c0 as char);
        out.push(c1 as char);
        out.push(if chunk.len() > 1 { c2 as char } else { '=' });
        out.push(if chunk.len() > 2 { c3 as char } else { '=' });
    }
    out
}

// ---------------------------------------------------------------------
// Screenshot outcome (per scene)
// ---------------------------------------------------------------------

/// The result of attempting to capture one scene's screenshot. Every
/// failure mode - missing binary, a scene that fails to load, no renderer
/// in a headless environment, a timeout, a capture/read error - collapses
/// to [`ScreenshotOutcome::NotCaptured`] with a short, specific reason;
/// nothing here is ever fatal to the overall command (see the module doc
/// comment).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenshotOutcome {
    Captured { data_uri: String },
    NotCaptured { reason: String },
}

// ---------------------------------------------------------------------
// HTML rendering
// ---------------------------------------------------------------------

const STYLE: &str = r#"
* { box-sizing: border-box; }
body {
    font-family: -apple-system, "Segoe UI", Helvetica, Arial, sans-serif;
    margin: 0;
    padding: 0;
    background: #f7f7f8;
    color: #1a1a1a;
}
header {
    background: #20232a;
    color: #fff;
    padding: 1.25rem 2rem;
}
header h1 { margin: 0 0 0.25rem 0; font-size: 1.4rem; }
header .meta { margin: 0; color: #c7c9cf; font-size: 0.9rem; }
main { padding: 1.5rem 2rem 3rem; max-width: 1100px; margin: 0 auto; }
.empty { font-style: italic; color: #555; }
section.scene {
    background: #fff;
    border: 1px solid #ddd;
    border-radius: 6px;
    margin-bottom: 1.5rem;
    padding: 1rem 1.25rem;
}
section.scene h2 {
    margin: 0 0 0.75rem 0;
    font-size: 1.1rem;
    border-bottom: 1px solid #eee;
    padding-bottom: 0.5rem;
}
.scene-path {
    font-family: "SFMono-Regular", Consolas, monospace;
    font-weight: normal;
    color: #666;
    font-size: 0.85rem;
}
.screenshot { margin-bottom: 0.75rem; }
.screenshot img { max-width: 100%; border: 1px solid #ccc; border-radius: 4px; display: block; }
.screenshot-status {
    font-size: 0.85rem;
    color: #8a6d00;
    background: #fff8e1;
    border: 1px solid #f2df9b;
    padding: 0.4rem 0.6rem;
    border-radius: 4px;
    display: inline-block;
    margin-bottom: 0.75rem;
}
.table-wrap { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
th, td { text-align: left; padding: 0.45rem 0.6rem; border-bottom: 1px solid #eee; vertical-align: top; }
th { color: #444; font-weight: 600; border-bottom: 2px solid #ddd; white-space: nowrap; }
td.string { font-weight: 600; color: #111; }
td.reference { font-family: Consolas, monospace; font-size: 0.8rem; color: #666; white-space: nowrap; }
"#;

/// Sort `records` by `(scene_path, line, node_path, property)` - see the
/// module doc comment's "HTML shape" section. The last two fields are
/// tie-breakers only (two different properties on the same node can share
/// a source line is not possible in `.tscn`'s one-property-per-line
/// syntax, but breaking ties explicitly keeps this fully deterministic
/// regardless of that).
fn sorted_records(records: &[TranslatableString]) -> Vec<&TranslatableString> {
    let mut sorted: Vec<&TranslatableString> = records.iter().collect();
    sorted.sort_by(|a, b| {
        a.scene_path
            .cmp(&b.scene_path)
            .then(a.line.cmp(&b.line))
            .then(a.node_path.cmp(&b.node_path))
            .then(a.property.cmp(&b.property))
    });
    sorted
}

/// One scene's worth of rows, ready to render as a `<section>`.
struct SceneSection<'a> {
    scene_path: &'a Path,
    screen: &'a str,
    rows: Vec<&'a TranslatableString>,
}

/// Group already-[`sorted_records`]-sorted records into one
/// [`SceneSection`] per distinct `scene_path`, preserving sorted order.
fn group_by_scene<'a>(sorted: &[&'a TranslatableString]) -> Vec<SceneSection<'a>> {
    let mut sections: Vec<SceneSection<'a>> = Vec::new();
    for &r in sorted {
        match sections.last_mut() {
            Some(last) if last.scene_path == r.scene_path.as_path() => last.rows.push(r),
            _ => sections.push(SceneSection {
                scene_path: &r.scene_path,
                screen: &r.screen,
                rows: vec![r],
            }),
        }
    }
    sections
}

/// A scene section's display label for its heading: `res_path` when known
/// (identical to what [`reference`] would use as its base), otherwise
/// `scene_path` (forward-slash-separated regardless of host path
/// separator).
fn scene_label(section: &SceneSection) -> String {
    section
        .rows
        .first()
        .and_then(|r| r.res_path.clone())
        .unwrap_or_else(|| section.scene_path.to_string_lossy().replace('\\', "/"))
}

fn render_scene_section(
    out: &mut String,
    section: &SceneSection,
    screenshots: Option<&HashMap<PathBuf, ScreenshotOutcome>>,
) {
    let label = scene_label(section);

    out.push_str("<section class=\"scene\">\n<h2>");
    out.push_str(&escape_html(section.screen));
    out.push_str(" <span class=\"scene-path\">");
    out.push_str(&escape_html(&label));
    out.push_str("</span></h2>\n");

    if let Some(map) = screenshots {
        match map.get(section.scene_path) {
            Some(ScreenshotOutcome::Captured { data_uri }) => {
                out.push_str("<div class=\"screenshot captured\">\n<img src=\"");
                out.push_str(&escape_html(data_uri));
                out.push_str("\" alt=\"Screenshot of ");
                out.push_str(&escape_html(&label));
                out.push_str("\">\n</div>\n");
            }
            Some(ScreenshotOutcome::NotCaptured { reason }) => {
                out.push_str("<p class=\"screenshot-status\">Screenshot not captured: ");
                out.push_str(&escape_html(reason));
                out.push_str("</p>\n");
            }
            None => {
                out.push_str("<p class=\"screenshot-status\">Screenshot not captured: screenshot not attempted</p>\n");
            }
        }
    }

    out.push_str("<div class=\"table-wrap\">\n<table>\n<thead><tr>");
    out.push_str("<th>String</th><th>Node Path</th><th>Node Type</th><th>Property</th><th>Reference</th>");
    out.push_str("</tr></thead>\n<tbody>\n");
    for row in &section.rows {
        out.push_str("<tr><td class=\"string\">");
        out.push_str(&escape_html_multiline(&row.text));
        out.push_str("</td><td>");
        out.push_str(&escape_html(&row.node_path));
        out.push_str("</td><td>");
        out.push_str(&escape_html(&row.node_type));
        out.push_str("</td><td>");
        out.push_str(&escape_html(&row.property));
        out.push_str("</td><td class=\"reference\">");
        out.push_str(&escape_html(&reference(row)));
        out.push_str("</td></tr>\n");
    }
    out.push_str("</tbody>\n</table>\n</div>\n</section>\n");
}

/// Render a full, standalone HTML document for `records` - see the module
/// doc comment's "HTML shape" section. `screenshots`: `None` when
/// `--screenshots` was not given (no screenshot markup is rendered at
/// all); `Some(map)` when it was, keyed by each record's `scene_path`
/// (a scene present in `records` but absent from `map` - which should not
/// happen in practice, [`capture_screenshots`] always populates one entry
/// per distinct scene - falls back to a generic "not attempted" note
/// rather than silently omitting the row).
fn render_html(records: &[TranslatableString], screenshots: Option<&HashMap<PathBuf, ScreenshotOutcome>>) -> String {
    let sorted = sorted_records(records);
    let sections = group_by_scene(&sorted);

    let mut out = String::new();
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<title>Translator Review - sg i18n shots</title>\n<style>");
    out.push_str(STYLE);
    out.push_str("</style>\n</head>\n<body>\n");

    out.push_str("<header>\n<h1>Translator Review</h1>\n<p class=\"meta\">Generated by sg i18n shots | ");
    out.push_str(&format!(
        "{} string(s) across {} scene(s)",
        records.len(),
        sections.len()
    ));
    if let Some(map) = screenshots {
        let captured = map
            .values()
            .filter(|s| matches!(s, ScreenshotOutcome::Captured { .. }))
            .count();
        let not_captured = map
            .values()
            .filter(|s| matches!(s, ScreenshotOutcome::NotCaptured { .. }))
            .count();
        out.push_str(&format!(
            " | Screenshots: {captured} captured, {not_captured} not captured"
        ));
    }
    out.push_str("</p>\n</header>\n<main>\n");

    if sections.is_empty() {
        out.push_str("<p class=\"empty\">No translatable strings were found.</p>\n");
    } else {
        for section in &sections {
            render_scene_section(&mut out, section, screenshots);
        }
    }

    out.push_str("</main>\n</body>\n</html>\n");
    out
}

// ---------------------------------------------------------------------
// Screenshot capture (best-effort, opt-in)
// ---------------------------------------------------------------------

/// GDScript run inside each target project by [`capture_group`]. Mirrors
/// [`crate::engine`]'s `VALIDATOR_SCRIPT` structure (a `SceneTree`
/// standalone script, a `SG-SHOT-RESULT`/`SG-SHOT-DONE` tab-separated
/// output protocol so [`parse_shot_output`] never has to guess which
/// output lines are ours among Godot's own startup/error noise).
///
/// For every `res://` scene path passed as a user argument after the
/// output directory (`args[0]`), attempts to load it, instantiate it, grab
/// a frame from the root viewport, and save it to disk under `out_dir`,
/// named after its zero-based index among the scene arguments.
///
/// This is expected not to produce an image at all under `--headless`:
/// Godot's headless mode uses a dummy rendering driver with no real
/// viewport texture, so `Viewport.get_texture()`/`Image` retrieval
/// routinely comes back empty or null - that specific, anticipated failure
/// is reported as the reason `"rendering unavailable in headless mode"`.
/// See the module doc comment - this is a documented constraint, not a bug
/// this script tries to work around.
const SCREENSHOT_SCRIPT: &str = r#"extends SceneTree

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	if args.is_empty():
		quit(1)
		return
	var out_dir: String = args[0]
	var any_fail := false
	for i in range(1, args.size()):
		var res_path: String = args[i]
		var idx: int = i - 1
		var reason := ""
		var res: Resource = ResourceLoader.load(res_path, "", ResourceLoader.CACHE_MODE_IGNORE)
		var packed := res as PackedScene
		if packed == null:
			reason = "scene failed to load"
		else:
			var inst: Node = packed.instantiate()
			if inst == null:
				reason = "scene failed to load"
			else:
				get_root().add_child(inst)
				var img: Image = null
				var tex := get_root().get_texture()
				if tex != null:
					img = tex.get_image()
				if img == null:
					reason = "rendering unavailable in headless mode"
				else:
					var out_path := "%s/%d.png" % [out_dir, idx]
					var err := img.save_png(out_path)
					if err != OK:
						reason = "failed to save screenshot (error code %d)" % err
				inst.queue_free()
		if reason == "":
			print("SG-SHOT-RESULT\tOK\t%d\t" % idx)
		else:
			reason = reason.replace("\t", " ").replace("\n", " ")
			print("SG-SHOT-RESULT\tFAIL\t%d\t%s" % [idx, reason])
			any_fail = true
	print("SG-SHOT-DONE\t%s" % ("FAIL" if any_fail else "OK"))
	quit(1 if any_fail else 0)
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShotLineOutcome {
    Ok,
    Fail(String),
}

/// Parse [`SCREENSHOT_SCRIPT`]'s stdout protocol into `scene-arg index ->
/// outcome`. Same tolerant-of-noise approach as
/// [`crate::engine::parse_validator_output`]: any line not prefixed with
/// the exact `SG-SHOT-RESULT` marker is ignored.
fn parse_shot_output(stdout: &str) -> HashMap<usize, ShotLineOutcome> {
    let mut results = HashMap::new();
    for line in stdout.lines() {
        let mut fields = line.splitn(4, '\t');
        if fields.next() != Some("SG-SHOT-RESULT") {
            continue;
        }
        let (Some(status), Some(idx_str)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Ok(idx) = idx_str.parse::<usize>() else {
            continue;
        };
        let reason = fields.next().unwrap_or("");
        let outcome = if status == "OK" {
            ShotLineOutcome::Ok
        } else {
            ShotLineOutcome::Fail(reason.to_string())
        };
        results.insert(idx, outcome);
    }
    results
}

static SHOT_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Write [`SCREENSHOT_SCRIPT`] to a fresh directory under the OS temp dir
/// (never inside the project being scanned) and create an `out/`
/// subdirectory for captured PNGs. Returns `(script_path, out_dir)`; both
/// live under the same parent directory, which the caller removes as a
/// unit once done.
fn write_screenshot_script() -> std::io::Result<(PathBuf, PathBuf)> {
    let n = SHOT_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("sg-i18n-shots-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let script_path = dir.join("sg_shots.gd");
    std::fs::write(&script_path, SCREENSHOT_SCRIPT)?;
    let out_dir = dir.join("out");
    std::fs::create_dir_all(&out_dir)?;
    Ok((script_path, out_dir))
}

/// Run one headless Godot launch covering every scene in `group`, and
/// derive a [`ScreenshotOutcome`] per file. Never returns an `Err` - every
/// environment-level failure (script could not be written, Godot could not
/// be launched) becomes a [`ScreenshotOutcome::NotCaptured`] for every
/// file in the group instead, since a screenshot failure must never fail
/// `sg i18n shots` itself (see the module doc comment).
fn capture_group(
    godot_bin: &Path,
    group: &engine::ProjectGroup,
    timeout: Duration,
) -> HashMap<PathBuf, ScreenshotOutcome> {
    let mut results = HashMap::new();

    let (script_path, out_dir) = match write_screenshot_script() {
        Ok(v) => v,
        Err(e) => {
            let reason = format!("failed to write temporary screenshot script: {e}");
            for (file, _) in &group.files {
                results.insert(file.clone(), ScreenshotOutcome::NotCaptured { reason: reason.clone() });
            }
            return results;
        }
    };
    let cleanup_dir = script_path.parent().map(Path::to_path_buf);

    let mut command = Command::new(godot_bin);
    command
        .arg("--headless")
        .arg("--path")
        .arg(&group.project_root)
        .arg("--script")
        .arg(&script_path)
        .arg("--")
        .arg(&out_dir);
    for (_, res_path) in &group.files {
        command.arg(res_path);
    }

    let run_result = engine::run_with_timeout(command, timeout);

    let outcome = match run_result {
        Ok(o) => o,
        Err(e) => {
            let reason = format!("failed to run Godot ('{}'): {e}", godot_bin.display());
            for (file, _) in &group.files {
                results.insert(file.clone(), ScreenshotOutcome::NotCaptured { reason: reason.clone() });
            }
            if let Some(dir) = cleanup_dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            return results;
        }
    };

    let parsed = parse_shot_output(&outcome.stdout);
    for (idx, (file, _res_path)) in group.files.iter().enumerate() {
        let file_outcome = match parsed.get(&idx) {
            Some(ShotLineOutcome::Ok) => {
                let png_path = out_dir.join(format!("{idx}.png"));
                match std::fs::read(&png_path) {
                    Ok(bytes) => ScreenshotOutcome::Captured {
                        data_uri: format!("data:image/png;base64,{}", base64_encode(&bytes)),
                    },
                    Err(e) => ScreenshotOutcome::NotCaptured {
                        reason: format!("screenshot reported OK but the PNG could not be read: {e}"),
                    },
                }
            }
            Some(ShotLineOutcome::Fail(reason)) => ScreenshotOutcome::NotCaptured { reason: reason.clone() },
            None if outcome.timed_out => ScreenshotOutcome::NotCaptured {
                reason: "timed out".to_string(),
            },
            None => ScreenshotOutcome::NotCaptured {
                reason: "Godot exited without reporting a screenshot result".to_string(),
            },
        };
        results.insert(file.clone(), file_outcome);
    }

    if let Some(dir) = cleanup_dir {
        let _ = std::fs::remove_dir_all(dir);
    }

    results
}

/// Attempt a best-effort screenshot for every distinct scene in `records`.
/// Always returns one entry per distinct `scene_path` - never an `Err` -
/// so the caller can render "not captured: <reason>" uniformly regardless
/// of *why* nothing was captured (see the module doc comment).
fn capture_screenshots(
    records: &[TranslatableString],
    godot_path: Option<&Path>,
    timeout: Duration,
) -> HashMap<PathBuf, ScreenshotOutcome> {
    let mut scene_paths: Vec<PathBuf> = Vec::new();
    {
        let mut seen: std::collections::HashSet<&Path> = std::collections::HashSet::new();
        for r in records {
            if seen.insert(r.scene_path.as_path()) {
                scene_paths.push(r.scene_path.clone());
            }
        }
    }
    if scene_paths.is_empty() {
        return HashMap::new();
    }

    let mut results = HashMap::new();

    let godot_bin = match engine::find_godot_binary(godot_path) {
        Ok(bin) => bin,
        Err(msg) => {
            for scene in scene_paths {
                results.insert(scene, ScreenshotOutcome::NotCaptured { reason: msg.clone() });
            }
            return results;
        }
    };

    let (groups, unrooted) = engine::group_by_project(&scene_paths);
    for file in unrooted {
        results.insert(
            file.clone(),
            ScreenshotOutcome::NotCaptured {
                reason: format!(
                    "no project.godot found in any ancestor of '{}'; cannot resolve a res:// path",
                    file.display()
                ),
            },
        );
    }

    for group in &groups {
        results.extend(capture_group(&godot_bin, group, timeout));
    }

    results
}

// ---------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------

/// Run `sg i18n shots`: expand `paths` the same way `sg check` does, scan
/// every scene file found ([`crate::i18n::scan`]), optionally attempt a
/// best-effort screenshot per scene, and write the resulting self-
/// contained HTML to `output`.
///
/// Exit codes: `0` every input file scanned cleanly (an empty result - no
/// translatable strings at all - is not a failure, and neither is a
/// screenshot that could not be captured: see the module doc comment),
/// `2` at least one input file failed to read or parse (matching `sg
/// check`/`sg i18n extract`'s parse-error exit code), `1` the HTML could
/// not be written to `output`. A missing `--output` is a clap usage error
/// before this function is ever called (clap's own usage-error exit code,
/// already `2` elsewhere in this codebase).
pub fn run(
    paths: &[PathBuf],
    output: &Path,
    take_screenshots: bool,
    godot_path: Option<&Path>,
    engine_timeout: u64,
) -> ExitCode {
    let files = collect_target_files(paths);
    let outcome = scan(&files);

    for err in &outcome.errors {
        eprintln!("error: {}: {}", err.file.display(), err.message);
    }

    let screenshot_map = if take_screenshots {
        Some(capture_screenshots(
            &outcome.records,
            godot_path,
            Duration::from_secs(engine_timeout),
        ))
    } else {
        None
    };

    let html = render_html(&outcome.records, screenshot_map.as_ref());

    if let Err(e) = std::fs::write(output, &html) {
        eprintln!("error: failed to write '{}': {e}", output.display());
        return ExitCode::FAILURE;
    }

    if outcome.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize as TestCounter, Ordering as TestOrdering};

    fn record(text: &str) -> TranslatableString {
        TranslatableString {
            text: text.to_string(),
            scene_path: PathBuf::from("scene.tscn"),
            res_path: Some("res://scene.tscn".to_string()),
            node_path: "VBox/Label".to_string(),
            node_type: "Label".to_string(),
            screen: "MainMenu".to_string(),
            property: "text".to_string(),
            line: 1,
        }
    }

    // -----------------------------------------------------------------
    // escape_html / escape_html_multiline
    // -----------------------------------------------------------------

    #[test]
    fn escape_html_escapes_lt_gt_amp_and_quote() {
        assert_eq!(escape_html("<b>&\"x\"</b>"), "&lt;b&gt;&amp;&quot;x&quot;&lt;/b&gt;");
    }

    #[test]
    fn escape_html_escapes_single_quote() {
        assert_eq!(escape_html("it's"), "it&#39;s");
    }

    #[test]
    fn escape_html_leaves_plain_text_unchanged() {
        assert_eq!(escape_html("Start Game"), "Start Game");
    }

    #[test]
    fn escape_html_multiline_converts_embedded_newline_to_br() {
        assert_eq!(escape_html_multiline("Line1\nLine2"), "Line1<br>\nLine2");
    }

    #[test]
    fn escape_html_multiline_still_escapes_html_special_characters() {
        assert_eq!(escape_html_multiline("<a>\n&b"), "&lt;a&gt;<br>\n&amp;b");
    }

    // -----------------------------------------------------------------
    // base64_encode
    // -----------------------------------------------------------------

    #[test]
    fn base64_encode_matches_rfc4648_test_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    // -----------------------------------------------------------------
    // render_html: structure
    // -----------------------------------------------------------------

    #[test]
    fn render_html_is_a_valid_looking_self_contained_document() {
        let html = render_html(&[record("Welcome")], None);
        assert!(html.starts_with("<!doctype html>"), "{html}");
        assert!(html.contains("<html lang=\"en\">"), "{html}");
        assert!(html.contains("<meta charset=\"utf-8\">"), "{html}");
        assert!(html.contains("<title>"), "{html}");
        assert!(html.contains("<style>"), "{html}");
        assert!(html.contains("<body>"), "{html}");
        assert!(html.ends_with("</html>\n"), "{html}");
        // No external resources of any kind.
        assert!(!html.contains("http://"), "{html}");
        assert!(!html.contains("https://"), "{html}");
        assert!(!html.contains("<link"), "{html}");
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn render_html_includes_every_string_and_context_field_escaped() {
        let r = TranslatableString {
            text: "Save & <Exit>".to_string(),
            scene_path: PathBuf::from("escape.tscn"),
            res_path: Some("res://escape.tscn".to_string()),
            node_path: "VBox/<Weird>".to_string(),
            node_type: "Label".to_string(),
            screen: "EscapeDemo".to_string(),
            property: "text".to_string(),
            line: 5,
        };
        let html = render_html(&[r], None);
        assert!(html.contains("Save &amp; &lt;Exit&gt;"), "{html}");
        assert!(!html.contains("Save & <Exit>"), "raw unescaped text leaked: {html}");
        assert!(html.contains("VBox/&lt;Weird&gt;"), "{html}");
        assert!(html.contains("Label"), "{html}");
        assert!(html.contains("EscapeDemo"), "{html}");
        assert!(html.contains("text"), "{html}");
        assert!(html.contains("res://escape.tscn:VBox/&lt;Weird&gt;"), "{html}");
    }

    #[test]
    fn render_html_groups_records_by_scene_into_one_heading_per_scene() {
        let a = TranslatableString {
            scene_path: PathBuf::from("a.tscn"),
            res_path: None,
            screen: "SceneA".to_string(),
            ..record("A")
        };
        let b = TranslatableString {
            scene_path: PathBuf::from("b.tscn"),
            res_path: None,
            screen: "SceneB".to_string(),
            ..record("B")
        };
        let html = render_html(&[a, b], None);
        assert_eq!(html.matches("<section class=\"scene\"").count(), 2, "{html}");
        assert!(html.contains("SceneA"), "{html}");
        assert!(html.contains("SceneB"), "{html}");
    }

    #[test]
    fn render_html_orders_scenes_by_scene_path_regardless_of_input_order() {
        let z = TranslatableString {
            scene_path: PathBuf::from("z_scene.tscn"),
            res_path: None,
            screen: "Z".to_string(),
            ..record("z")
        };
        let a = TranslatableString {
            scene_path: PathBuf::from("a_scene.tscn"),
            res_path: None,
            screen: "A".to_string(),
            ..record("a")
        };
        // Passed in "wrong" (z before a) order; output must still be
        // sorted by scene_path.
        let html = render_html(&[z, a], None);
        let a_pos = html.find("a_scene.tscn").unwrap();
        let z_pos = html.find("z_scene.tscn").unwrap();
        assert!(a_pos < z_pos, "{html}");
    }

    #[test]
    fn render_html_summary_line_reports_total_strings_and_scene_count() {
        let a1 = TranslatableString {
            scene_path: PathBuf::from("a.tscn"),
            res_path: None,
            line: 1,
            ..record("A1")
        };
        let a2 = TranslatableString {
            scene_path: PathBuf::from("a.tscn"),
            res_path: None,
            line: 2,
            ..record("A2")
        };
        let b1 = TranslatableString {
            scene_path: PathBuf::from("b.tscn"),
            res_path: None,
            line: 1,
            ..record("B1")
        };
        let html = render_html(&[a1, a2, b1], None);
        assert!(html.contains("3 string(s) across 2 scene(s)"), "{html}");
    }

    #[test]
    fn render_html_empty_input_yields_a_valid_document_with_a_no_strings_state() {
        let html = render_html(&[], None);
        assert!(html.starts_with("<!doctype html>"), "{html}");
        assert!(html.contains("No translatable strings were found"), "{html}");
        assert!(html.contains("0 string(s) across 0 scene(s)"), "{html}");
        assert!(!html.contains("<section class=\"scene\""), "{html}");
    }

    // -----------------------------------------------------------------
    // render_html: screenshot states
    // -----------------------------------------------------------------

    #[test]
    fn render_html_omits_all_screenshot_markup_when_screenshots_were_not_requested() {
        // Static CSS class definitions (in <style>, always present and
        // harmless whether used or not) are not the concern here - the
        // concern is that no actual screenshot *content* is rendered when
        // --screenshots was never requested.
        let html = render_html(&[record("Welcome")], None);
        assert!(!html.contains("<div class=\"screenshot"), "{html}");
        assert!(!html.contains("<p class=\"screenshot-status\""), "{html}");
        assert!(!html.contains("Screenshot not captured"), "{html}");
        assert!(!html.contains("Screenshots:"), "{html}");
        assert!(!html.contains("<img"), "{html}");
    }

    #[test]
    fn render_html_shows_not_captured_with_reason_when_screenshot_missing() {
        let r = record("Welcome");
        let mut map = HashMap::new();
        map.insert(
            r.scene_path.clone(),
            ScreenshotOutcome::NotCaptured {
                reason: "rendering unavailable in headless mode".to_string(),
            },
        );
        let html = render_html(&[r], Some(&map));
        assert!(html.contains("Screenshot not captured"), "{html}");
        assert!(html.contains("rendering unavailable in headless mode"), "{html}");
        assert!(!html.contains("<img"), "{html}");
    }

    #[test]
    fn render_html_escapes_the_not_captured_reason() {
        let r = record("Welcome");
        let mut map = HashMap::new();
        map.insert(
            r.scene_path.clone(),
            ScreenshotOutcome::NotCaptured {
                reason: "load failed: <bad & \"weird\">".to_string(),
            },
        );
        let html = render_html(&[r], Some(&map));
        assert!(
            html.contains("load failed: &lt;bad &amp; &quot;weird&quot;&gt;"),
            "{html}"
        );
    }

    #[test]
    fn render_html_embeds_a_captured_screenshot_as_a_data_uri_image() {
        let r = record("Welcome");
        let mut map = HashMap::new();
        map.insert(
            r.scene_path.clone(),
            ScreenshotOutcome::Captured {
                data_uri: "data:image/png;base64,AAAA".to_string(),
            },
        );
        let html = render_html(&[r], Some(&map));
        assert!(html.contains("<img src=\"data:image/png;base64,AAAA\""), "{html}");
        assert!(!html.contains("Screenshot not captured"), "{html}");
    }

    #[test]
    fn render_html_screenshot_summary_counts_captured_and_not_captured() {
        let a = TranslatableString {
            scene_path: PathBuf::from("a.tscn"),
            res_path: None,
            ..record("A")
        };
        let b = TranslatableString {
            scene_path: PathBuf::from("b.tscn"),
            res_path: None,
            ..record("B")
        };
        let mut map = HashMap::new();
        map.insert(
            PathBuf::from("a.tscn"),
            ScreenshotOutcome::Captured {
                data_uri: "data:image/png;base64,AAAA".to_string(),
            },
        );
        map.insert(
            PathBuf::from("b.tscn"),
            ScreenshotOutcome::NotCaptured {
                reason: "timed out".to_string(),
            },
        );
        let html = render_html(&[a, b], Some(&map));
        assert!(html.contains("Screenshots: 1 captured, 1 not captured"), "{html}");
    }

    #[test]
    fn render_html_screenshot_summary_absent_when_screenshots_not_requested() {
        let html = render_html(&[record("Welcome")], None);
        assert!(!html.contains("Screenshots:"), "{html}");
    }

    // -----------------------------------------------------------------
    // parse_shot_output
    // -----------------------------------------------------------------

    #[test]
    fn parse_shot_output_parses_ok_and_fail_lines_and_ignores_noise() {
        let stdout = concat!(
            "Godot Engine v4.7.1.stable.official - https://godotengine.org\n",
            "SG-SHOT-RESULT\tOK\t0\t\n",
            "ERROR: some unrelated engine error\n",
            "SG-SHOT-RESULT\tFAIL\t1\trendering unavailable in headless mode\n",
            "SG-SHOT-DONE\tFAIL\n",
        );
        let parsed = parse_shot_output(stdout);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get(&0), Some(&ShotLineOutcome::Ok));
        assert_eq!(
            parsed.get(&1),
            Some(&ShotLineOutcome::Fail(
                "rendering unavailable in headless mode".to_string()
            ))
        );
    }

    #[test]
    fn parse_shot_output_empty_or_garbage_yields_no_results() {
        assert!(parse_shot_output("").is_empty());
        assert!(parse_shot_output("not a marker line\nneither is this\n").is_empty());
    }

    // -----------------------------------------------------------------
    // capture_screenshots: graceful fallback
    // -----------------------------------------------------------------

    static TMP_COUNTER: TestCounter = TestCounter::new(0);

    fn fresh_temp_dir(label: &str) -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, TestOrdering::SeqCst);
        let dir = std::env::temp_dir().join(format!("sg-i18n-shots-test-{label}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn capture_screenshots_with_no_records_returns_an_empty_map() {
        assert!(capture_screenshots(&[], None, Duration::from_secs(5)).is_empty());
    }

    #[test]
    fn capture_screenshots_reports_a_missing_godot_binary_as_not_captured_for_every_scene() {
        let dir = fresh_temp_dir("badbin");
        let missing_bin = dir.join("does-not-exist.exe");
        let r = record("Welcome");
        let map = capture_screenshots(std::slice::from_ref(&r), Some(&missing_bin), Duration::from_secs(5));
        assert_eq!(map.len(), 1);
        match map.get(&r.scene_path) {
            Some(ScreenshotOutcome::NotCaptured { reason }) => {
                assert!(reason.contains("--godot-path"), "{reason}");
                assert!(reason.contains("does not point to an executable file"), "{reason}");
            }
            other => panic!("expected NotCaptured, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capture_screenshots_reports_an_unrooted_scene_as_not_captured() {
        let dir = fresh_temp_dir("unrooted");
        let scene = dir.join("orphan.tscn");
        std::fs::write(&scene, "").unwrap();
        // A dummy stand-in "binary" that only needs to resolve as an
        // existing file - it is never actually spawned, since the scene
        // has no project.godot ancestor and is filtered out before any
        // process would be launched.
        let dummy_bin = dir.join("fake_godot.exe");
        std::fs::write(&dummy_bin, "").unwrap();

        let r = TranslatableString {
            scene_path: scene.clone(),
            ..record("Orphan")
        };
        let map = capture_screenshots(&[r], Some(&dummy_bin), Duration::from_secs(5));
        match map.get(&scene) {
            Some(ScreenshotOutcome::NotCaptured { reason }) => {
                assert!(reason.contains("project.godot"), "{reason}");
            }
            other => panic!("expected NotCaptured, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capture_screenshots_deduplicates_scenes_with_multiple_strings() {
        let dir = fresh_temp_dir("dedup");
        let missing_bin = dir.join("does-not-exist.exe");
        let a = record("A");
        let b = TranslatableString { line: 2, ..record("B") }; // same scene_path as `a`
        let map = capture_screenshots(&[a, b], Some(&missing_bin), Duration::from_secs(5));
        assert_eq!(map.len(), 1, "{map:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
