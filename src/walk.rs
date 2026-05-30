//! File discovery — the one walk policy shared by source scanning, PO collection,
//! and the audit pass.
//!
//! Owns: a `WalkBuilder` per root (honouring `.gitignore`), pruning of
//! `ignore_dirs` by directory name, cross-root dedup by absolute path, and a
//! deterministic final sort — the determinism the parallel folds downstream rely
//! on. Callers express only *what counts as a hit* via an `accept` closure that
//! maps each [`FileEntry`] to an optional value.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

/// A file the walk found, in the three views a caller might need: the `root` it
/// was found under, its path `rel`ative to that root, and the full `abs` path.
/// Source scanning keys off `abs`; PO glob matching needs `rel` (patterns are
/// anchored at the root) with `abs` as a fallback.
pub struct FileEntry<'a> {
    pub root: &'a Path,
    pub rel: &'a Path,
    pub abs: &'a Path,
}

/// Walk `roots`, pruning `ignore_dirs` (on top of `.gitignore`), and collect each
/// file the `accept` closure maps to `Some`. Files are deduplicated by absolute
/// path across roots and returned in deterministic (sorted-by-path) order, so the
/// result is independent of root order and walk scheduling.
pub fn find_files<T>(
    roots: &[PathBuf],
    ignore_dirs: &[String],
    accept: impl Fn(&FileEntry) -> Option<T>,
) -> Result<Vec<T>> {
    let ignored: HashSet<String> = ignore_dirs.iter().cloned().collect();
    let mut found: Vec<(PathBuf, T)> = Vec::new();
    let mut seen = HashSet::new();

    for root in roots {
        let ignored = ignored.clone();
        let mut walker = WalkBuilder::new(root);
        walker.filter_entry(move |entry| {
            // Prune ignored directories by name; always keep files.
            if entry.file_type().is_some_and(|t| t.is_dir())
                && let Some(name) = entry.file_name().to_str()
            {
                return !ignored.contains(name);
            }
            true
        });

        for result in walker.build() {
            let entry = result.with_context(|| format!("error walking {}", root.display()))?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let abs = entry.path();
            let rel = abs.strip_prefix(root).unwrap_or(abs);
            let file = FileEntry { root, rel, abs };
            if let Some(value) = accept(&file)
                && seen.insert(abs.to_path_buf())
            {
                found.push((abs.to_path_buf(), value));
            }
        }
    }

    // Deterministic order so any downstream parallel fold is reproducible.
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found.into_iter().map(|(_, value)| value).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dead-poets-walk-{tag}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
    }

    /// `accept` selects hits; `ignore_dirs` prunes whole subtrees by name.
    #[test]
    fn accept_selects_and_ignore_dirs_prune() {
        let dir = temp_dir("accept");
        write(&dir, "keep.php", "");
        write(&dir, "skip.txt", "");
        write(&dir, "vendor/buried.php", "");

        let hits = find_files(std::slice::from_ref(&dir), &["vendor".to_string()], |e| {
            (e.abs.extension().and_then(|x| x.to_str()) == Some("php")).then(|| e.abs.to_path_buf())
        })
        .unwrap();

        assert_eq!(hits.len(), 1);
        assert!(hits[0].ends_with("keep.php"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The closure sees `rel` anchored at the root it was found under.
    #[test]
    fn rel_is_relative_to_root() {
        let dir = temp_dir("rel");
        write(&dir, "a/b/c.po", "");

        let rels = find_files(std::slice::from_ref(&dir), &[], |e| {
            Some(e.rel.to_path_buf())
        })
        .unwrap();

        assert_eq!(rels, vec![PathBuf::from("a/b/c.po")]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Overlapping roots yield each file once, in sorted order.
    #[test]
    fn dedups_overlapping_roots_and_sorts() {
        let dir = temp_dir("dedup");
        write(&dir, "z.php", "");
        write(&dir, "a.php", "");
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        write(&sub, "m.php", "");

        // `dir` and `dir/nested` overlap: nested/m.php is reachable from both.
        let roots = vec![dir.clone(), sub.clone()];
        let names: Vec<String> = find_files(&roots, &[], |e| {
            Some(e.abs.file_name().unwrap().to_string_lossy().into_owned())
        })
        .unwrap();

        // m.php appears once despite two reachable roots; output is path-sorted
        // (a.php < nested/m.php < z.php by full path).
        assert_eq!(names, vec!["a.php", "m.php", "z.php"]);

        std::fs::remove_dir_all(&dir).ok();
    }
}
