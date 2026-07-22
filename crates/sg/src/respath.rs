//! Resolves a `res://`-relative path against a real Godot project
//! directory tree, distinguishing "exists with exactly this case" from
//! "exists, but some path component has different case" from "does not
//! exist at all" - used by the `missing-ext-resource-path` and
//! `ext-resource-path-case-mismatch` static rules in [`crate::rules`].
//!
//! ## Why not just `Path::exists()`
//!
//! On a case-insensitive filesystem (default on Windows and macOS),
//! `Path::exists()` returns `true` for `res://Scripts/player.gd` even when
//! the file on disk is actually `scripts/Player.gd` - the project *looks*
//! fine to whoever is developing on those platforms, but the same path
//! fails to resolve on Linux and in exported builds, where the filesystem
//! is case-sensitive. [`check_res_path`] walks each path component through
//! a real directory listing ([`std::fs::read_dir`]) and compares names
//! case-sensitively first, falling back to a case-insensitive match only
//! to detect (and name) the mismatch - so it reaches the same verdict on
//! every platform, which is the entire point.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The result of resolving one `res://`-relative path against a project
/// directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathCheck {
    /// Every path component matched a directory entry exactly, including
    /// case.
    Exact,
    /// Every path component matched a directory entry when compared
    /// case-insensitively, but at least one differed in exact case.
    /// `actual_relative` is the on-disk path (correct case throughout,
    /// `/`-separated) with the same number of components.
    CaseMismatch { actual_relative: String },
    /// Every path component resolved to a real, existing directory entry
    /// (exactly, or via the case-insensitive fallback - `actual_relative`
    /// reflects whichever casing is actually on disk), but the final
    /// component names a directory, not a file. Godot's `ResourceLoader`
    /// can never load a directory as a resource, so a path like
    /// `res://scripts` (where `scripts/` is a directory) is just as
    /// unusable as one that doesn't exist at all, even though it passes a
    /// bare [`std::path::Path::exists`] check.
    IsDirectory { actual_relative: String },
    /// Some path component has no directory entry at all, even
    /// case-insensitively - the path does not exist on disk under any
    /// casing.
    Missing,
}

/// Per-run cache of directory listings, keyed by absolute directory path.
/// A single `.tscn`/`.tres` file's `ext_resource` sections commonly share
/// several leading path components (e.g. a common `scripts/` or
/// `assets/` directory), so without this, checking N resources in the
/// same directory would call [`std::fs::read_dir`] on it N times.
#[derive(Default)]
pub struct DirCache {
    listings: HashMap<PathBuf, Option<Vec<OsString>>>,
}

impl DirCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Directory entry names for `dir`, or `None` if `dir` cannot be
    /// listed (does not exist, is not a directory, or is unreadable).
    /// Computed once per distinct `dir` and cached for the lifetime of
    /// this `DirCache`.
    fn list(&mut self, dir: &Path) -> Option<&[OsString]> {
        let entry = self.listings.entry(dir.to_path_buf()).or_insert_with(|| {
            std::fs::read_dir(dir)
                .ok()
                .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
        });
        entry.as_deref()
    }
}

/// Lexically normalize a `/`-split component list the way Godot's own
/// `String::simplify_path()` does: `.` components are dropped (they name
/// "this directory") and a `..` component cancels the previous real
/// component instead of being looked up as a literal directory entry
/// named `".."` (which - unlike `.`/`..` themselves inside a real
/// directory listing - never appears in [`std::fs::read_dir`]'s output,
/// so without this pass `res://a/../b.gd` would be (mis)reported as
/// missing even when `res://b.gd` exists). A leading `..` with nothing
/// left to cancel would have to reach above `project_root`, which is
/// never a valid `res://` path, so `None` is returned for that case.
fn normalize_components<'a>(raw: impl Iterator<Item = &'a str>) -> Option<Vec<&'a str>> {
    let mut out: Vec<&str> = Vec::new();
    for component in raw {
        match component {
            "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// Resolve `res_relative` (a `res://` path with the `res://` prefix
/// already stripped, e.g. `"scripts/player.gd"`) against `project_root`,
/// walking one path component at a time through `cache`'s directory
/// listings.
///
/// Each component is looked up in its parent directory's listing first
/// for an exact (case-sensitive) match; only when that fails is a
/// case-insensitive match attempted, and only to produce
/// [`PathCheck::CaseMismatch`] (or, if even that fails, to conclude
/// [`PathCheck::Missing`]). A component that matches exactly never
/// triggers the case-insensitive fallback at all, so a fully-correct path
/// costs exactly one exact-match scan per component.
///
/// Before any of that, `res_relative` is split on `/` and lexically
/// normalized ([`normalize_components`]): empty components (from a
/// trailing slash or a doubled `//`), `.` components, and `..`
/// components are resolved the same way Godot itself resolves them,
/// rather than being looked up as literal directory entries.
pub fn check_res_path(project_root: &Path, res_relative: &str, cache: &mut DirCache) -> PathCheck {
    let Some(components) = normalize_components(res_relative.split('/').filter(|s| !s.is_empty())) else {
        return PathCheck::Missing;
    };
    if components.is_empty() {
        return PathCheck::Missing;
    }

    let mut current_dir = project_root.to_path_buf();
    let mut actual_components: Vec<String> = Vec::with_capacity(components.len());
    let mut case_mismatch = false;

    for component in components {
        let Some(entries) = cache.list(&current_dir) else {
            return PathCheck::Missing;
        };

        if let Some(exact) = entries.iter().find(|e| e.to_str() == Some(component)) {
            let name = exact.to_string_lossy().into_owned();
            current_dir.push(&name);
            actual_components.push(name);
            continue;
        }

        let case_insensitive = entries.iter().find(|e| {
            e.to_str()
                .map(|s| s.to_lowercase() == component.to_lowercase())
                .unwrap_or(false)
        });
        match case_insensitive {
            Some(found) => {
                case_mismatch = true;
                let name = found.to_string_lossy().into_owned();
                current_dir.push(&name);
                actual_components.push(name);
            }
            None => return PathCheck::Missing,
        }
    }

    // Every component matched a real entry, but Godot can only ever load a
    // *file* through `ext_resource` - a path that resolves to a directory
    // is just as unusable as one that resolves to nothing, so it takes
    // priority over reporting a (moot) exact-case match.
    if current_dir.is_dir() {
        return PathCheck::IsDirectory {
            actual_relative: actual_components.join("/"),
        };
    }

    if case_mismatch {
        PathCheck::CaseMismatch {
            actual_relative: actual_components.join("/"),
        }
    } else {
        PathCheck::Exact
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn fresh_temp_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("sg-respath-test-{label}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn exact_case_match_is_exact() {
        let root = fresh_temp_dir("exact");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(check_res_path(&root, "scripts/player.gd", &mut cache), PathCheck::Exact);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn filename_case_mismatch_is_detected() {
        let root = fresh_temp_dir("file-case");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("Player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        let result = check_res_path(&root, "scripts/player.gd", &mut cache);
        assert_eq!(
            result,
            PathCheck::CaseMismatch {
                actual_relative: "scripts/Player.gd".to_string()
            }
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directory_case_mismatch_is_detected() {
        let root = fresh_temp_dir("dir-case");
        fs::create_dir_all(root.join("Scripts")).unwrap();
        fs::write(root.join("Scripts").join("player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        let result = check_res_path(&root, "scripts/player.gd", &mut cache);
        assert_eq!(
            result,
            PathCheck::CaseMismatch {
                actual_relative: "Scripts/player.gd".to_string()
            }
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nonexistent_file_in_existing_directory_is_missing() {
        let root = fresh_temp_dir("missing-file");
        fs::create_dir_all(root.join("scripts")).unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts/does_not_exist.gd", &mut cache),
            PathCheck::Missing
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nonexistent_directory_is_missing() {
        let root = fresh_temp_dir("missing-dir");

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "no_such_dir/player.gd", &mut cache),
            PathCheck::Missing
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_relative_path_is_missing() {
        let root = fresh_temp_dir("empty");
        let mut cache = DirCache::new();
        assert_eq!(check_res_path(&root, "", &mut cache), PathCheck::Missing);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cache_reuses_a_directory_listing_across_calls() {
        // Read the listing once (populating the cache), then delete the
        // file from disk. A second lookup through the *same* cache must
        // still report Exact, proving the listing was actually cached
        // rather than re-read from disk each time.
        let root = fresh_temp_dir("cache-reuse");
        fs::create_dir_all(root.join("scripts")).unwrap();
        let file = root.join("scripts").join("player.gd");
        fs::write(&file, "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(check_res_path(&root, "scripts/player.gd", &mut cache), PathCheck::Exact);

        fs::remove_file(&file).unwrap();
        assert_eq!(
            check_res_path(&root, "scripts/player.gd", &mut cache),
            PathCheck::Exact,
            "second lookup should hit the cached (now stale) listing, not re-read the directory"
        );
        fs::remove_dir_all(&root).ok();
    }

    // -----------------------------------------------------------------
    // Adversarial path-component edge cases.
    // -----------------------------------------------------------------

    #[test]
    fn path_pointing_at_a_directory_is_reported_as_is_directory() {
        // `res://scripts` (no trailing slash) resolves to a real directory
        // entry - `Path::exists()`-style logic would call that "found",
        // but Godot's `ResourceLoader` can never load a directory as a
        // resource, so this must not be reported as `Exact`.
        let root = fresh_temp_dir("dir-as-file");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts", &mut cache),
            PathCheck::IsDirectory {
                actual_relative: "scripts".to_string()
            }
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn trailing_slash_on_a_directory_is_also_reported_as_is_directory() {
        // A trailing slash normalizes away to the same component list as
        // the no-slash case above; both must be caught identically.
        let root = fresh_temp_dir("dir-trailing-slash");
        fs::create_dir_all(root.join("scripts")).unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts/", &mut cache),
            PathCheck::IsDirectory {
                actual_relative: "scripts".to_string()
            }
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directory_pointed_at_via_case_insensitive_match_is_still_is_directory() {
        // Case-mismatch resolution and the directory check must compose:
        // the directory verdict wins, but still carries the corrected
        // on-disk casing so the reported message stays accurate.
        let root = fresh_temp_dir("dir-case-mismatch");
        fs::create_dir_all(root.join("Scripts")).unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts", &mut cache),
            PathCheck::IsDirectory {
                actual_relative: "Scripts".to_string()
            }
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn embedded_double_slash_is_normalized_like_a_single_slash() {
        // Godot's own path simplification collapses doubled slashes; a
        // literal directory entry named "" can never exist, so without
        // filtering empty components this would wrongly report Missing.
        let root = fresh_temp_dir("double-slash");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts//player.gd", &mut cache),
            PathCheck::Exact
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn leading_dot_component_is_a_no_op_like_godots_simplify_path() {
        // `res://./scripts/player.gd` names the exact same file as
        // `res://scripts/player.gd` - a literal directory entry named "."
        // is never listed by `read_dir`, so without normalization this
        // would wrongly report Missing.
        let root = fresh_temp_dir("dot-component");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "./scripts/player.gd", &mut cache),
            PathCheck::Exact
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dotdot_component_cancels_the_preceding_component() {
        // `res://scripts/../scripts/player.gd` names the exact same file
        // as `res://scripts/player.gd` once simplified - `..` is never a
        // literal directory entry either.
        let root = fresh_temp_dir("dotdot-component");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts/../scripts/player.gd", &mut cache),
            PathCheck::Exact
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dotdot_escaping_above_the_project_root_is_missing() {
        // A `..` with nothing left to cancel would have to reach above
        // `project_root`, which can never be a valid `res://` path.
        let root = fresh_temp_dir("dotdot-escape");
        let mut cache = DirCache::new();
        assert_eq!(check_res_path(&root, "../outside.gd", &mut cache), PathCheck::Missing);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn backslash_is_not_a_path_separator_and_never_resolves() {
        // Godot's res:// paths use '/' exclusively; a literal backslash is
        // just an ordinary (and here, non-matching) character within a
        // single component, never a separator to split on.
        let root = fresh_temp_dir("backslash");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("player.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, r"scripts\player.gd", &mut cache),
            PathCheck::Missing
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn filenames_with_spaces_and_percent_signs_resolve_normally() {
        let root = fresh_temp_dir("space-percent");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("my file 100%.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts/my file 100%.gd", &mut cache),
            PathCheck::Exact
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unicode_filenames_resolve_normally() {
        let root = fresh_temp_dir("unicode");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts").join("プレイヤー.gd"), "").unwrap();

        let mut cache = DirCache::new();
        assert_eq!(
            check_res_path(&root, "scripts/プレイヤー.gd", &mut cache),
            PathCheck::Exact
        );
        fs::remove_dir_all(&root).ok();
    }
}
