//! Expands CLI path arguments (files or directories) into a sorted,
//! deduplicated list of `.tscn`/`.tres` files, recursing into
//! directories; and resolves the Godot project root for a given file -
//! shared by `sg check --engine` ([`crate::engine`]) and the static
//! `res://`-path-on-disk rules ([`crate::rules`]) so both agree on
//! exactly what "this file's `res://` root" means.

use std::path::{Path, PathBuf};

pub fn collect_target_files(inputs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for input in inputs {
        collect_one(input, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn collect_one(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(path) {
            Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
            Err(_) => Vec::new(),
        };
        entries.sort();
        for entry in entries {
            collect_one(&entry, out);
        }
    } else if is_target_file(path) {
        out.push(path.to_path_buf());
    }
}

fn is_target_file(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("tscn") | Some("tres"))
}

/// Walk upward from `file`'s directory looking for `project.godot`,
/// returning the first ancestor directory that contains one. Resolves
/// `file` to an absolute path first (lexically, via [`std::path::absolute`]
/// - no filesystem access, no symlink resolution) so relative inputs like
///   a bare `scene.tscn` (whose `Path::parent()` would otherwise be the
///   empty path, terminating the walk after a single check) are handled the
///   same as any other input.
pub fn find_project_root(file: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(file).ok()?;
    let mut dir = abs.parent()?.to_path_buf();
    loop {
        if dir.join("project.godot").is_file() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn recurses_into_directories_and_filters_by_extension() {
        let dir = std::env::temp_dir().join(format!("sg-paths-test-{}", std::process::id()));
        let nested = dir.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.join("a.tscn"), "").unwrap();
        fs::write(dir.join("b.txt"), "").unwrap();
        fs::write(nested.join("c.tres"), "").unwrap();

        let found = collect_target_files(std::slice::from_ref(&dir));
        assert_eq!(found, vec![dir.join("a.tscn"), nested.join("c.tres")]);

        fs::remove_dir_all(&dir).ok();
    }

    // -- find_project_root ------------------------------------------------
    //
    // Moved here (unchanged) from crates/sg/src/engine.rs when
    // `find_project_root` became shared between `sg check --engine` and
    // the static ext_resource-path-on-disk rules.

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn fresh_temp_dir(label: &str) -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("sg-paths-test-{label}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_project_root_several_levels_up() {
        let root = fresh_temp_dir("proj-root");
        fs::write(root.join("project.godot"), "").unwrap();
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("scene.tscn");
        fs::write(&file, "").unwrap();

        let found = find_project_root(&file).unwrap();
        assert_eq!(
            std::path::absolute(&found).unwrap(),
            std::path::absolute(&root).unwrap()
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn returns_none_when_no_project_godot_exists() {
        let dir = fresh_temp_dir("no-proj");
        let file = dir.join("scene.tscn");
        fs::write(&file, "").unwrap();
        // `dir` itself has no project.godot, and (barring an actual Godot
        // project somewhere above the OS temp dir, which would be
        // pathological) neither does anything above it.
        assert_eq!(find_project_root(&file), None);
        fs::remove_dir_all(&dir).ok();
    }
}
