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
pub fn check_res_path(project_root: &Path, res_relative: &str, cache: &mut DirCache) -> PathCheck {
    let components: Vec<&str> = res_relative.split('/').filter(|s| !s.is_empty()).collect();
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
}
