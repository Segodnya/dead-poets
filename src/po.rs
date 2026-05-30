//! PO key universe — the set of keys we check for liveness.
//!
//! The universe is the **union of msgids across all locales** (PLAN: "Build the
//! key universe as the union of msgids across all locales"). A key is modelled
//! as `(msgctxt?, msgid, msgid_plural?)` so context/plural projects are
//! supported generically, even though `example_repo` exercises neither.
//!
//! Obsolete (`#~`) entries never reach us — polib treats them as comments.
//! Fuzzy entries are skipped explicitly. msgids arrive already decoded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use polib::catalog::Catalog;
use polib::po_file;

/// A PO catalog key. Equality/hash over the full tuple so the union across
/// locales deduplicates identical entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoKey {
    pub msgctxt: Option<String>,
    pub msgid: String,
    pub msgid_plural: Option<String>,
}

/// The deduplicated key universe.
#[derive(Debug, Default)]
pub struct PoIndex {
    keys: HashSet<PoKey>,
}

impl PoIndex {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Iterate the unique keys.
    pub fn keys(&self) -> impl Iterator<Item = &PoKey> {
        self.keys.iter()
    }

    /// Whether any key carries this exact msgid (singular or plural form).
    pub fn contains_msgid(&self, msgid: &str) -> bool {
        self.keys.iter().any(|k| {
            k.msgid == msgid || k.msgid_plural.as_deref() == Some(msgid)
        })
    }
}

/// Find all PO/POT files under `roots` matching any of the `po_patterns` globs,
/// skipping `ignore_dirs` (on top of `.gitignore`). Patterns are matched against
/// the path relative to each root, falling back to the absolute path.
pub fn collect_po_files(
    roots: &[PathBuf],
    po_patterns: &[String],
    ignore_dirs: &[String],
) -> Result<Vec<PathBuf>> {
    let mut builder = GlobSetBuilder::new();
    for pattern in po_patterns {
        builder.add(
            Glob::new(pattern).with_context(|| format!("invalid po_pattern glob: {pattern}"))?,
        );
    }
    let globs = builder.build().context("failed to build PO glob set")?;

    let ignored: HashSet<String> = ignore_dirs.iter().cloned().collect();
    let mut found = Vec::new();
    let mut seen = HashSet::new();

    for root in roots {
        let ignored = ignored.clone();
        let mut walker = WalkBuilder::new(root);
        walker.filter_entry(move |entry| {
            // Prune ignored directories by name; always keep files.
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !ignored.contains(name);
                }
            }
            true
        });

        for result in walker.build() {
            let entry = result.with_context(|| format!("error walking {}", root.display()))?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path);
            if globs.is_match(rel) || globs.is_match(path) {
                let owned = path.to_path_buf();
                if seen.insert(owned.clone()) {
                    found.push(owned);
                }
            }
        }
    }

    found.sort();
    Ok(found)
}

/// Build the key universe from the given PO files.
pub fn build_index(files: &[&Path]) -> Result<PoIndex> {
    let mut keys = HashSet::new();
    for file in files {
        let catalog = parse_catalog(file)?;
        for message in catalog.messages() {
            // Skip fuzzy entries — they aren't active translations.
            if message.flags().is_fuzzy() {
                continue;
            }
            let msgid = message.msgid();
            // Skip the metadata header (empty msgid).
            if msgid.is_empty() {
                continue;
            }
            let msgid_plural = if message.is_plural() {
                message.msgid_plural().ok().map(|s| s.to_string())
            } else {
                None
            };
            let msgctxt = message.msgctxt().map(|s| s.to_string());
            keys.insert(PoKey {
                msgctxt,
                msgid: msgid.to_string(),
                msgid_plural,
            });
        }
    }
    Ok(PoIndex { keys })
}

/// Convenience: collect + build in one call.
pub fn load_index(
    roots: &[PathBuf],
    po_patterns: &[String],
    ignore_dirs: &[String],
) -> Result<PoIndex> {
    let files = collect_po_files(roots, po_patterns, ignore_dirs)?;
    if files.is_empty() {
        return Err(anyhow!(
            "no PO files matched {:?} under roots {:?}",
            po_patterns,
            roots
        ));
    }
    let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
    build_index(&refs)
}

/// Parse one catalog, converting both parse errors and polib panics (it can
/// panic on malformed lines) into a clean error naming the file, so one bad
/// catalog never aborts the whole scan with a raw panic.
fn parse_catalog(file: &Path) -> Result<Catalog> {
    let path = file.to_path_buf();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| po_file::parse(&path)));
    match outcome {
        Ok(Ok(catalog)) => Ok(catalog),
        Ok(Err(e)) => Err(anyhow!("failed to parse PO file {}: {}", file.display(), e)),
        Err(_) => Err(anyhow!(
            "failed to parse PO file {} (malformed content)",
            file.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    const HEADER: &str = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dead-poets-po-{tag}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Three locales: each file collected; a key in only one locale still lands
    /// in the index (union semantics); a plural key keeps both forms.
    #[test]
    fn union_across_three_locales() {
        let dir = temp_dir("union");
        write(
            &dir,
            "fr/LC_MESSAGES/messages.po",
            &format!("{HEADER}msgid \"shared\"\nmsgstr \"partage\"\n"),
        );
        write(
            &dir,
            "de/LC_MESSAGES/messages.po",
            &format!("{HEADER}msgid \"shared\"\nmsgstr \"geteilt\"\n\nmsgid \"only_de\"\nmsgstr \"nur\"\n"),
        );
        write(
            &dir,
            "es/LC_MESSAGES/messages.po",
            &format!(
                "{HEADER}msgid \"1 apple\"\nmsgid_plural \"%d apples\"\nmsgstr[0] \"manzana\"\nmsgstr[1] \"manzanas\"\n"
            ),
        );

        let files = collect_po_files(
            &[dir.clone()],
            &["**/*.po".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(files.len(), 3, "all three locale files collected");

        let refs: Vec<&Path> = files.iter().map(|p| p.as_path()).collect();
        let index = build_index(&refs).unwrap();

        // "shared" appears in 2 locales -> one key. "only_de" in 1 locale -> present.
        assert!(index.contains_msgid("shared"));
        assert!(index.contains_msgid("only_de"));

        // plural key keeps both forms
        let plural = index
            .keys()
            .find(|k| k.msgid == "1 apple")
            .expect("plural key present");
        assert_eq!(plural.msgid_plural.as_deref(), Some("%d apples"));

        // union dedup: shared + only_de + plural = 3 unique keys
        assert_eq!(index.len(), 3);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Obsolete (`#~`) and fuzzy entries are excluded; active entries remain.
    #[test]
    fn skips_obsolete_and_fuzzy() {
        let dir = temp_dir("skip");
        let body = format!(
            "{HEADER}\
             msgid \"active\"\nmsgstr \"a\"\n\n\
             #, fuzzy\nmsgid \"fuzzy_key\"\nmsgstr \"f\"\n\n\
             #~ msgid \"obsolete_key\"\n#~ msgstr \"o\"\n"
        );
        let file = write(&dir, "messages.po", &body);
        let index = build_index(&[file.as_path()]).unwrap();

        assert!(index.contains_msgid("active"));
        assert!(!index.contains_msgid("fuzzy_key"), "fuzzy excluded");
        assert!(!index.contains_msgid("obsolete_key"), "obsolete excluded");
        assert_eq!(index.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// polib returns decoded msgids: `\n` in the PO source becomes a real newline.
    #[test]
    fn msgids_are_decoded() {
        let dir = temp_dir("decode");
        let file = write(&dir, "messages.po", &format!("{HEADER}msgid \"a\\nb\"\nmsgstr \"x\"\n"));
        let index = build_index(&[file.as_path()]).unwrap();

        let key = index.keys().next().unwrap();
        assert_eq!(key.msgid, "a\nb", "escape sequence decoded to real newline");
        assert!(key.msgid.contains('\n'));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A malformed PO (missing the metadata header) errors with the file path.
    #[test]
    fn broken_po_errors_with_path() {
        let dir = temp_dir("broken");
        // No metadata header block -> polib rejects it.
        let file = write(&dir, "broken.po", "msgid \"orphan\"\nmsgstr \"x\"\n");
        let err = build_index(&[file.as_path()]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("broken.po"), "error names the file: {msg}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
