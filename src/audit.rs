//! Dead-bucket audit — an opt-in **trust score** over the `Dead` list.
//!
//! The classifier (`liveness`) is deterministic and biases hard toward keep:
//! a key is `Dead` only when no modeled call, guard, whitelist, or verbatim
//! source literal references it. That still leaves a fair question — *how much
//! do we trust the Dead list?* This module answers it by grepping every dead
//! msgid against the **raw source** (not just AST string literals) and bucketing
//! each key by the strongest residual trace it leaves:
//!
//! - [`Trace::Substring`] — the full msgid occurs as a raw substring somewhere
//!   (a comment, HTML text, a heredoc, or inside a larger string). A real tell.
//! - [`Trace::Skeleton`] — only for keys carrying `printf`/brace placeholders and
//!   only when there is no substring hit: stripping the placeholders yields static
//!   fragments (each ≥ `min_guard_len`) that **all** appear in source — the mark
//!   of a `sprintf`-assembled key.
//! - [`Trace::None`] — no trace of any kind: high-confidence dead.
//!
//! The audit is **advisory only**: it never reclassifies a key and never affects
//! the exit code. It runs a single multi-pattern pass (Aho–Corasick) per file,
//! parallelized like [`crate::scan::aggregate`], so the result is independent of
//! thread count.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use aho_corasick::AhoCorasick;
use anyhow::Result;
use rayon::prelude::*;
use regex::Regex;

use crate::po::PoKey;
use crate::scan::collect_source_files;

/// The strongest residual trace a dead key leaves in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trace {
    /// The full msgid appears as a raw substring somewhere in source.
    Substring,
    /// A placeholder key whose static fragments all appear in source.
    Skeleton,
    /// No trace of any kind — high-confidence dead.
    None,
}

/// Aggregated audit result over the `Dead` bucket.
#[derive(Debug, Default)]
pub struct AuditReport {
    pub dead_total: usize,
    pub no_trace: usize,
    pub substring: usize,
    pub skeleton: usize,
    /// The keys that *did* leave a trace (the recheck list) with their tier.
    /// `None`-trace keys are omitted — they are already the actionable Dead list.
    pub traced: Vec<(PoKey, Trace)>,
}

/// The placeholder grammar: python-named `%(name)s`, printf `%[argnum$][flags]
/// [width][.prec]conv`, and brace `{0}` / `{name}`. Compiled once.
fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"%\([^)]+\)[a-zA-Z]|%(?:\d+\$)?[-+ 0#']*\d*(?:\.\d+)?[a-zA-Z]|\{[^}]*\}")
            .expect("valid placeholder regex")
    })
}

/// Split a msgid into its static fragments around placeholders, keeping only
/// fragments of at least `min_len` characters. Returns empty when the msgid
/// carries no placeholder at all — the skeleton tier applies only to such keys.
pub fn skeleton_fragments(msgid: &str, min_len: usize) -> Vec<String> {
    let re = placeholder_re();
    if !re.is_match(msgid) {
        return Vec::new();
    }
    re.split(msgid)
        .filter(|s| s.chars().count() >= min_len)
        .map(|s| s.to_string())
        .collect()
}

/// Which dead key a search pattern belongs to, and in what role.
#[derive(Clone, Copy)]
enum Role {
    /// The whole msgid of dead key `index`.
    Whole(usize),
    /// One skeleton fragment of dead key `index`.
    Fragment(usize),
}

/// Audit the `dead` keys against the raw source under `roots`.
///
/// Mirrors [`crate::scan`] for file discovery and parallelism. Pure measurement —
/// the caller's verdicts and exit code are untouched.
pub fn audit(
    dead: &[&PoKey],
    roots: &[PathBuf],
    source_extensions: &[String],
    ignore_dirs: &[String],
    min_guard_len: usize,
) -> Result<AuditReport> {
    let files = collect_source_files(roots, source_extensions, ignore_dirs)?;

    // Build the pattern set: each dead msgid (Whole) plus its skeleton fragments.
    // Pattern ids are assigned in push order, so `roles[id]` describes pattern id.
    let mut patterns: Vec<String> = Vec::new();
    let mut roles: Vec<Role> = Vec::new();
    let mut frag_counts: Vec<usize> = vec![0; dead.len()];
    for (i, key) in dead.iter().enumerate() {
        if key.msgid.is_empty() {
            continue;
        }
        patterns.push(key.msgid.clone());
        roles.push(Role::Whole(i));
        let frags = skeleton_fragments(&key.msgid, min_guard_len);
        frag_counts[i] = frags.len();
        for f in frags {
            patterns.push(f);
            roles.push(Role::Fragment(i));
        }
    }

    // Empty pattern set (no dead keys, or all empty) -> nothing to trace.
    if patterns.is_empty() {
        return Ok(AuditReport {
            dead_total: dead.len(),
            no_trace: dead.len(),
            ..Default::default()
        });
    }

    let ac = AhoCorasick::new(&patterns).expect("valid pattern set");

    // One overlapping pass per file; union the matched pattern ids. The union is
    // order-independent, so the result is identical regardless of thread count.
    let matched: HashSet<usize> = files
        .par_iter()
        .map(|(_, path)| {
            let mut local = HashSet::new();
            if let Ok(src) = std::fs::read_to_string(path) {
                for m in ac.find_overlapping_iter(&src) {
                    local.insert(m.pattern().as_usize());
                }
            }
            local
        })
        .reduce(HashSet::new, |mut acc, set| {
            acc.extend(set);
            acc
        });

    // Fold matched patterns back onto their dead keys.
    let mut whole_hit = vec![false; dead.len()];
    let mut frag_hits = vec![0usize; dead.len()];
    for pid in &matched {
        match roles[*pid] {
            Role::Whole(i) => whole_hit[i] = true,
            Role::Fragment(i) => frag_hits[i] += 1,
        }
    }

    let mut report = AuditReport {
        dead_total: dead.len(),
        ..Default::default()
    };
    for (i, key) in dead.iter().enumerate() {
        let trace = if whole_hit[i] {
            Trace::Substring
        } else if frag_counts[i] > 0 && frag_hits[i] == frag_counts[i] {
            Trace::Skeleton
        } else {
            Trace::None
        };
        match trace {
            Trace::Substring => {
                report.substring += 1;
                report.traced.push(((*key).clone(), trace));
            }
            Trace::Skeleton => {
                report.skeleton += 1;
                report.traced.push(((*key).clone(), trace));
            }
            Trace::None => report.no_trace += 1,
        }
    }
    // Stable, reviewable order for the recheck list.
    report
        .traced
        .sort_by(|a, b| (&a.0.msgctxt, &a.0.msgid).cmp(&(&b.0.msgctxt, &b.0.msgid)));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(msgid: &str) -> PoKey {
        PoKey {
            msgctxt: None,
            msgid: msgid.to_string(),
            msgid_plural: None,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dead-poets-audit-{tag}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// printf, python-named, and brace placeholders all split into the surrounding
    /// static fragments; a placeholder-free msgid yields no fragments.
    #[test]
    fn skeleton_fragments_split_on_placeholders() {
        assert_eq!(
            skeleton_fragments("Deleted %d contacts", 3),
            vec!["Deleted ".to_string(), " contacts".to_string()]
        );
        assert_eq!(
            skeleton_fragments("%1$s reacted to message", 3),
            vec![" reacted to message".to_string()]
        );
        assert_eq!(
            skeleton_fragments("Welcome {name} home", 3),
            vec!["Welcome ".to_string(), " home".to_string()]
        );
        // No placeholder -> empty (skeleton tier does not apply).
        assert!(skeleton_fragments("plain key", 3).is_empty());
        // Fragments below the threshold are dropped (" : " is too short here).
        assert!(skeleton_fragments("%s: %s", 3).is_empty());
    }

    /// One key per tier: substring (verbatim in a comment), skeleton (placeholder
    /// fragments present, msgid itself absent), and none (no trace at all).
    #[test]
    fn audit_buckets_each_tier() {
        let dir = temp_dir("tiers");
        std::fs::write(
            dir.join("a.php"),
            "<?php // note: Verbatim dead phrase here\n\
             $x = 'Deleted ' . $n . ' contacts now';\n\
             notADeadKey('whatever'); ?>",
        )
        .unwrap();

        let k_sub = key("Verbatim dead phrase");
        let k_skel = key("Deleted %d contacts");
        let k_none = key("Nowhere To Be Found");
        let dead = vec![&k_sub, &k_skel, &k_none];

        let report = audit(
            &dead,
            &[dir.clone()],
            &["php".to_string()],
            &[],
            3,
        )
        .unwrap();

        assert_eq!(report.dead_total, 3);
        assert_eq!(report.substring, 1);
        assert_eq!(report.skeleton, 1);
        assert_eq!(report.no_trace, 1);

        let tier = |id: &str| report.traced.iter().find(|(k, _)| k.msgid == id).map(|(_, t)| *t);
        assert_eq!(tier("Verbatim dead phrase"), Some(Trace::Substring));
        assert_eq!(tier("Deleted %d contacts"), Some(Trace::Skeleton));
        // None-trace keys are omitted from the recheck list.
        assert_eq!(tier("Nowhere To Be Found"), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A substring hit wins over a skeleton hit for the same key.
    #[test]
    fn substring_beats_skeleton() {
        let dir = temp_dir("precedence");
        // The full msgid (with %d) appears verbatim, e.g. in a data table.
        std::fs::write(dir.join("t.php"), "<?php $m = 'Deleted %d contacts'; ?>").unwrap();

        let k = key("Deleted %d contacts");
        let report = audit(&[&k], &[dir.clone()], &["php".to_string()], &[], 3).unwrap();
        assert_eq!(report.substring, 1);
        assert_eq!(report.skeleton, 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The union of per-file matches is order-independent: single- and
    /// multi-threaded runs over the same files produce an identical report.
    #[test]
    fn deterministic_across_thread_counts() {
        let dir = temp_dir("determinism");
        for i in 0..6 {
            std::fs::write(
                dir.join(format!("f{i}.php")),
                format!("<?php $a = 'trace key {i}'; ?>"),
            )
            .unwrap();
        }
        let keys: Vec<PoKey> = (0..6).map(|i| key(&format!("trace key {i}"))).collect();
        let dead: Vec<&PoKey> = keys.iter().collect();

        let run = |threads: usize| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| audit(&dead, &[dir.clone()], &["php".to_string()], &[], 3).unwrap())
        };
        let single = run(1);
        let multi = run(4);
        assert_eq!(single.substring, multi.substring);
        assert_eq!(single.no_trace, multi.no_trace);
        assert_eq!(single.substring, 6);

        std::fs::remove_dir_all(&dir).ok();
    }
}
