//! Source scanning — walk the roots and extract usage in parallel
//! (PLAN Addendum 2 §6).
//!
//! Files are processed with `rayon`; each worker holds a thread-local
//! [`ParserPool`] reused across files. Per-file results are collected in input
//! (sorted) order and folded sequentially into a [`Usage`], so the aggregate is
//! **identical regardless of thread count** — a property the tests assert.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use rayon::prelude::*;

use crate::config::CallSpec;
use crate::extract::{ExtractResult, ParserPool, SourceLang, extract_with_pool};
use crate::liveness::Usage;
use crate::walk::find_files;

thread_local! {
    static POOL: RefCell<ParserPool> = RefCell::new(ParserPool::new());
}

/// Find scannable source files under `roots`, mapped to their language.
pub fn collect_source_files(
    roots: &[PathBuf],
    source_extensions: &[String],
    ignore_dirs: &[String],
) -> Result<Vec<(SourceLang, PathBuf)>> {
    let exts: HashSet<String> = source_extensions.iter().map(|e| e.to_lowercase()).collect();
    find_files(roots, ignore_dirs, |entry| {
        let ext = entry
            .abs
            .extension()
            .and_then(|e| e.to_str())?
            .to_lowercase();
        if !exts.contains(&ext) {
            return None;
        }
        let lang = SourceLang::from_extension(&ext)?;
        Some((lang, entry.abs.to_path_buf()))
    })
}

/// Scan all sources under `roots` and aggregate their translation usage.
pub fn scan_sources(
    roots: &[PathBuf],
    source_extensions: &[String],
    ignore_dirs: &[String],
    calls: &[CallSpec],
    min_guard_len: usize,
) -> Result<Usage> {
    let files = collect_source_files(roots, source_extensions, ignore_dirs)?;
    let usage = aggregate(&files, calls, min_guard_len);
    Ok(usage)
}

/// Extract from already-collected files and fold into a [`Usage`]. Separated so
/// tests can drive a fixed file list under different thread pools.
pub fn aggregate(
    files: &[(SourceLang, PathBuf)],
    calls: &[CallSpec],
    min_guard_len: usize,
) -> Usage {
    // Parallel per-file extraction; `collect` preserves input order.
    let results: Vec<(&'static str, ExtractResult)> = files
        .par_iter()
        .map(|(lang, path)| {
            let source = std::fs::read_to_string(path).unwrap_or_default();
            let res = POOL.with(|cell| {
                extract_with_pool(&mut cell.borrow_mut(), *lang, &source, calls, min_guard_len)
            });
            (lang.family_label(), res)
        })
        .collect();

    let mut usage = Usage::default();
    for (label, res) in results {
        usage.add(label, res);
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CallKind, CallSpec};
    use std::path::Path;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dead-poets-scan-{tag}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn calls() -> Vec<CallSpec> {
        vec![
            CallSpec {
                lang: "php".into(),
                kind: CallKind::Function,
                name: "i18n".into(),
                receiver: None,
                key_arg_index: 0,
            },
            CallSpec {
                lang: "php".into(),
                kind: CallKind::Method,
                name: "get".into(),
                receiver: Some(vec!["i18n".into()]),
                key_arg_index: 0,
            },
            CallSpec {
                lang: "js".into(),
                kind: CallKind::Function,
                name: "i18n".into(),
                receiver: None,
                key_arg_index: 0,
            },
            CallSpec {
                lang: "twig".into(),
                kind: CallKind::Filter,
                name: "i18n".into(),
                receiver: None,
                key_arg_index: 0,
            },
        ]
    }

    /// Normalize a Usage into a comparable, order-independent shape.
    fn normalize(u: &Usage) -> (Vec<String>, Vec<String>, Vec<(String, usize)>) {
        let mut lits: Vec<String> = u.literals.iter().cloned().collect();
        lits.sort();
        let mut guards: Vec<String> = u.guards.iter().map(|g| format!("{g:?}")).collect();
        guards.sort();
        let blind: Vec<(String, usize)> = u.blind.iter().map(|(k, v)| (k.clone(), *v)).collect();
        (lits, guards, blind)
    }

    /// Single-threaded and multi-threaded runs over the same files produce an
    /// identical literal/guard/blind aggregate.
    #[test]
    fn single_and_multi_threaded_identical() {
        let dir = temp_dir("determinism");
        let mut files = Vec::new();
        // A spread of files across languages, enough to exercise >1 worker.
        for i in 0..4 {
            files.push((
                SourceLang::Php,
                write(
                    &dir,
                    &format!("a{i}.php"),
                    &format!("<?php i18n('php_{i}'); $i18n->get('get_{i}'); i18n(\"role_$x\"); ?>"),
                ),
            ));
            files.push((
                SourceLang::Js,
                write(
                    &dir,
                    &format!("b{i}.js"),
                    &format!("i18n('js_{i}'); i18n(`cf_${{x}}`); i18n($y);"),
                ),
            ));
            files.push((
                SourceLang::Twig,
                write(
                    &dir,
                    &format!("c{i}.twig"),
                    "{{ 'tw'|i18n }} {{ var|i18n }}",
                ),
            ));
        }
        files.sort_by(|a, b| a.1.cmp(&b.1));
        let calls = calls();

        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| aggregate(&files, &calls, 3));
        let multi = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap()
            .install(|| aggregate(&files, &calls, 3));

        assert_eq!(normalize(&single), normalize(&multi));

        // Sanity: content is what we expect, not empty.
        let (lits, guards, blind) = normalize(&single);
        assert!(lits.contains(&"php_0".to_string()));
        assert!(lits.contains(&"js_0".to_string()));
        assert!(lits.contains(&"tw".to_string()));
        assert!(guards.iter().any(|g| g.contains("cf_")));
        assert!(guards.iter().any(|g| g.contains("role_")));
        // js blind: i18n($y) x4 ; twig blind: {{ var|i18n }} x4
        assert_eq!(blind, vec![("js".to_string(), 4), ("twig".to_string(), 4)]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// File collection respects extensions and ignore_dirs.
    #[test]
    fn collect_respects_extensions_and_ignores() {
        let dir = temp_dir("collect");
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        write(&dir, "keep.php", "<?php ?>");
        write(&dir, "skip.txt", "nope");
        write(&dir.join("vendor"), "ignored.php", "<?php ?>");

        let files = collect_source_files(
            std::slice::from_ref(&dir),
            &["php".to_string(), "twig".to_string()],
            &["vendor".to_string()],
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].1.ends_with("keep.php"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
