//! Shared foundation for the `sg i18n` command family.
//!
//! `sg i18n extract` and `sg i18n budget` are the first two of a planned
//! set - `sg i18n shots` (per-screen screenshots for translator review)
//! and `sg i18n check` (CI gate combining the above) are expected to
//! follow. All of them start from the same question - "for every node in
//! these scene files, what is its path, its type, and which screen does
//! it belong to?" - so that walk lives here, once, as [`scan_document`]
//! (`extract`'s translatable-text properties) and, with the same
//! `build_node_graph`-based approach applied to control-geometry
//! properties instead, in [`budget::scan`]. Both commands therefore always
//! agree on what a node's path is - neither re-derives it a second,
//! possibly divergent way.
//!
//! Node path / instanced-node resolution itself is not reimplemented here
//! either: it is the exact same [`crate::nodegraph`] logic `crate::rules`
//! uses for its own structural checks, so `sg check` and `sg i18n` can
//! never disagree about what a node's path is.

pub mod budget;
pub mod extract;

use std::path::{Path, PathBuf};

use scenegraph_core::{parse_complete, Document};

use crate::nodegraph::{attr_str, build_node_graph, node_full_path};
use crate::paths::find_project_root;

/// One translatable string pulled out of a `.tscn` node property, with
/// enough context attached for a translator (or a future `sg i18n check`)
/// to find and judge it: which file, which node, which screen, which
/// property, and the source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatableString {
    /// The string value as decoded (unescaped) from the `.tscn` source.
    pub text: String,
    /// The scene file it came from, exactly as passed/expanded on the
    /// command line (not canonicalized).
    pub scene_path: PathBuf,
    /// `res://...` form of `scene_path`, if a `project.godot` ancestor was
    /// found (see [`crate::paths::find_project_root`]); `None` otherwise.
    pub res_path: Option<String>,
    /// Root-relative node path within the scene (`"."`,
    /// `"VBox/StartButton"`, etc.) - see [`crate::nodegraph::node_full_path`].
    pub node_path: String,
    /// The node's own `type` attribute, or empty for an instanced (or
    /// otherwise type-less) node section.
    pub node_type: String,
    /// The scene's root node name - the conventional "screen"/window
    /// identity a translator recognizes.
    pub screen: String,
    /// Which property this string came from (`"text"`, `"tooltip_text"`,
    /// etc.) - see [`TRANSLATABLE_PROPERTIES`].
    pub property: String,
    /// 1-based source line of the property, for reference/diagnostic
    /// precision.
    pub line: usize,
}

/// Node property names whose non-empty string values are extracted as
/// translatable strings in v1. Deliberately small and explicit - these
/// are the properties Godot's own UI classes use for player-facing text
/// (`Label`/`Button`/`CheckBox`/`RichTextLabel` `text`; any `Control`'s
/// `tooltip_text`; `LineEdit`/`TextEdit` `placeholder_text`; `Window` (and
/// subclasses) `title`; `AcceptDialog` (and subclasses) `dialog_text`) -
/// and meant to be extended in place as coverage grows; nothing else in
/// this module keys off these specific names.
///
/// Out of scope for v1: array/items-valued text properties, such as
/// `OptionButton`/`ItemList`'s `items`, which pack label text together
/// with icon/id/metadata into one packed array rather than a single
/// string value. Extracting those cleanly needs its own decoding step and
/// is a tracked future extension, not bolted on here.
pub const TRANSLATABLE_PROPERTIES: &[&str] = &["text", "tooltip_text", "placeholder_text", "title", "dialog_text"];

/// One file that could not be scanned at all: a read failure (missing
/// file, not valid UTF-8) or a parse failure. A file appearing here
/// contributes nothing to a [`ScanOutcome::records`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub file: PathBuf,
    pub message: String,
}

/// The result of scanning a set of scene files: every translatable string
/// found, in encounter order (file order among `files`, then node
/// declaration order within a file, then property declaration order
/// within a node), plus one [`ScanError`] per file that could not be read
/// or parsed. A failed file is simply skipped for `records`; it is not
/// fatal to the files that did scan cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanOutcome {
    pub records: Vec<TranslatableString>,
    pub errors: Vec<ScanError>,
}

/// Scan `files` (already-expanded scene file paths - see
/// [`crate::paths::collect_target_files`]) for every v1 translatable
/// string. This is the entry point every `sg i18n extract`-style command
/// calls; it owns reading and parsing so callers never have to duplicate
/// `sg`'s read/parse error conventions.
pub fn scan(files: &[PathBuf]) -> ScanOutcome {
    let mut outcome = ScanOutcome::default();
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
            Ok(doc) => outcome.records.extend(scan_document(&doc, file)),
            Err(e) => outcome.errors.push(ScanError {
                file: file.clone(),
                message: format!("parse error: {e}"),
            }),
        }
    }
    outcome
}

/// Scan a single already-parsed `doc` (read from `scene_path`) for
/// translatable strings. Exposed separately from [`scan`] so a caller
/// that already has a parsed [`Document`] (e.g. a future `sg i18n check`
/// sharing a document with `sg check`) never has to parse it twice.
pub fn scan_document(doc: &Document, scene_path: &Path) -> Vec<TranslatableString> {
    let sections = doc.sections();
    let graph = build_node_graph(&sections);
    let screen = graph
        .roots
        .first()
        .and_then(|&r| attr_str(&sections[r], "name"))
        .unwrap_or("")
        .to_string();
    let res_path = resolve_res_path(scene_path);

    let mut out = Vec::new();
    for &i in &graph.node_indices {
        let section = &sections[i];
        let name = attr_str(section, "name").unwrap_or("");
        let parent_attr = attr_str(section, "parent");
        let node_path = node_full_path(name, parent_attr);
        let node_type = attr_str(section, "type").unwrap_or("").to_string();

        for property in &section.properties {
            if !TRANSLATABLE_PROPERTIES.contains(&property.key.as_str()) {
                continue;
            }
            // Only string values are in scope - a property line that
            // fails to parse, or parses to something other than a plain
            // string/StringName (e.g. a malformed hand-edit), is silently
            // skipped rather than treated as an error: this is an
            // extraction pass, not a validator (that is `sg check`'s
            // job).
            let Ok(value) = parse_complete(&property.raw_value) else {
                continue;
            };
            let Some(text) = value.as_str() else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            out.push(TranslatableString {
                text: text.to_string(),
                scene_path: scene_path.to_path_buf(),
                res_path: res_path.clone(),
                node_path: node_path.clone(),
                node_type: node_type.clone(),
                screen: screen.clone(),
                property: property.key.clone(),
                line: property.line,
            });
        }
    }
    out
}

/// `res://...` form of `scene_path`, if it sits inside a discoverable
/// Godot project (see [`find_project_root`]); `None` otherwise. Always
/// forward-slash-separated, regardless of host path separator.
fn resolve_res_path(scene_path: &Path) -> Option<String> {
    let root = find_project_root(scene_path)?;
    let abs_file = std::path::absolute(scene_path).ok()?;
    let rel = abs_file.strip_prefix(&root).ok()?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    Some(format!("res://{rel_str}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_str(src: &str) -> Vec<TranslatableString> {
        let doc = Document::parse(src).unwrap();
        scan_document(&doc, Path::new("scene.tscn"))
    }

    #[test]
    fn extracts_label_text() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"Title\" type=\"Label\" parent=\".\"]\n",
            "text = \"Welcome\"\n",
        );
        let records = scan_str(src);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.text, "Welcome");
        assert_eq!(r.node_path, "Title");
        assert_eq!(r.node_type, "Label");
        assert_eq!(r.screen, "Main");
        assert_eq!(r.property, "text");
        assert_eq!(r.line, 6);
    }

    #[test]
    fn extracts_button_text_and_tooltip() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"Start\" type=\"Button\" parent=\".\"]\n",
            "text = \"Start Game\"\n",
            "tooltip_text = \"Begin your adventure\"\n",
        );
        let records = scan_str(src);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].property, "text");
        assert_eq!(records[0].text, "Start Game");
        assert_eq!(records[1].property, "tooltip_text");
        assert_eq!(records[1].text, "Begin your adventure");
    }

    #[test]
    fn extracts_placeholder_text() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"NameInput\" type=\"LineEdit\" parent=\".\"]\n",
            "placeholder_text = \"Enter your name\"\n",
        );
        let records = scan_str(src);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].property, "placeholder_text");
        assert_eq!(records[0].text, "Enter your name");
    }

    #[test]
    fn extracts_window_title_and_dialog_text() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"Popup\" type=\"Window\" parent=\".\"]\n",
            "title = \"Settings\"\n",
            "\n",
            "[node name=\"Confirm\" type=\"AcceptDialog\" parent=\".\"]\n",
            "dialog_text = \"Are you sure?\"\n",
        );
        let records = scan_str(src);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].property, "title");
        assert_eq!(records[0].text, "Settings");
        assert_eq!(records[0].node_type, "Window");
        assert_eq!(records[1].property, "dialog_text");
        assert_eq!(records[1].text, "Are you sure?");
        assert_eq!(records[1].node_type, "AcceptDialog");
    }

    #[test]
    fn skips_empty_strings() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"Hidden\" type=\"Label\" parent=\".\"]\n",
            "text = \"\"\n",
        );
        assert!(scan_str(src).is_empty());
    }

    #[test]
    fn skips_properties_outside_the_translatable_set() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "\n",
            "[node name=\"Icon\" type=\"Sprite2D\" parent=\".\"]\n",
            "texture_filter = \"disabled\"\n",
        );
        assert!(scan_str(src).is_empty());
    }

    #[test]
    fn node_path_is_root_relative_and_screen_is_the_scenes_root_name() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"MainMenu\" type=\"Control\"]\n",
            "\n",
            "[node name=\"VBox\" type=\"VBoxContainer\" parent=\".\"]\n",
            "\n",
            "[node name=\"StartButton\" type=\"Button\" parent=\"VBox\"]\n",
            "text = \"Start Game\"\n",
        );
        let records = scan_str(src);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].node_path, "VBox/StartButton");
        assert_eq!(records[0].screen, "MainMenu");
    }

    #[test]
    fn resolves_res_path_when_a_project_godot_ancestor_exists() {
        let root = std::env::temp_dir().join(format!("sg-i18n-test-respath-{}", std::process::id()));
        std::fs::create_dir_all(root.join("ui")).unwrap();
        std::fs::write(root.join("project.godot"), "").unwrap();
        let scene = root.join("ui").join("main_menu.tscn");
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"MainMenu\" type=\"Control\"]\n",
            "text = \"Welcome\"\n",
        );
        std::fs::write(&scene, src).unwrap();

        let doc = Document::parse(src).unwrap();
        let records = scan_document(&doc, &scene);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].res_path.as_deref(), Some("res://ui/main_menu.tscn"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn res_path_is_none_without_a_project_godot_ancestor() {
        let dir = std::env::temp_dir().join(format!("sg-i18n-test-no-respath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let scene = dir.join("main_menu.tscn");
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"MainMenu\" type=\"Control\"]\n",
            "text = \"Welcome\"\n",
        );
        std::fs::write(&scene, src).unwrap();

        let doc = Document::parse(src).unwrap();
        let records = scan_document(&doc, &scene);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].res_path, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_reports_a_read_error_for_a_missing_file() {
        let outcome = scan(&[PathBuf::from("does_not_exist_at_all.tscn")]);
        assert!(outcome.records.is_empty());
        assert_eq!(outcome.errors.len(), 1);
    }
}
