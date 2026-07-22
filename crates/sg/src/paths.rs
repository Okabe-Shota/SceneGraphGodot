//! Expands CLI path arguments (files or directories) into a sorted,
//! deduplicated list of `.tscn`/`.tres` files, recursing into
//! directories.

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
}
