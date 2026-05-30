//! Liveness classification (PLAN Addendum 2 §2, §3).
//!
//! Signals, in precedence order — a PO key is **Alive** if it has a whitelist,
//! literal, or guard match; live keys carry `alive_via` so "alive via guard" (the
//! old "low confidence") is expressed on the single status axis. A whitelisted
//! key is Alive via `Whitelist` — the explicit escape hatch for keys static
//! analysis cannot see (DB/config/external).
//!
//! Failing all of those, a key is **Suspect** if its msgid still appears verbatim
//! as a string literal *somewhere* in the source (just not in a call we model) —
//! the strong tell of a key dispatched dynamically through a data table, enum, or
//! factory. It is not a confirmed reference, so it is not `Alive`; but it is not
//! `Dead` either. Only a key with no reference of any kind is **Dead**.
//!
//! The per-language **blind summary** travels alongside and is never hidden.

use std::collections::{BTreeMap, HashSet};

use crate::extract::ExtractResult;
use crate::guard::Guard;
use crate::po::{PoIndex, PoKey};

/// Why a key is considered alive. Precedence: Whitelist > Literal > Guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliveVia {
    Literal,
    Guard,
    Whitelist,
}

/// A key's liveness status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Alive(AliveVia),
    /// Not referenced by any modeled call, but the msgid appears verbatim as a
    /// source string literal — likely dispatched dynamically. Verify, don't delete.
    Suspect,
    Dead,
}

/// The verdict for a single PO key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyVerdict {
    pub key: PoKey,
    pub status: Status,
}

/// Aggregated translation usage across all scanned source files.
#[derive(Debug, Default)]
pub struct Usage {
    pub literals: HashSet<String>,
    pub guards: Vec<Guard>,
    /// Blind call-site counts keyed by language family label.
    pub blind: BTreeMap<String, usize>,
    /// Every static string literal seen anywhere in the scanned source. Drives
    /// the `Suspect` tier.
    pub source_literals: HashSet<String>,
}

impl Usage {
    /// Fold one file's extraction result into the aggregate, attributing its
    /// blind sites to the given language family label.
    pub fn add(&mut self, lang_label: &str, result: ExtractResult) {
        self.literals.extend(result.literals);
        self.guards.extend(result.guards);
        if result.blind > 0 {
            *self.blind.entry(lang_label.to_string()).or_default() += result.blind;
        }
        self.source_literals.extend(result.source_literals);
    }
}

/// The full liveness result: per-key verdicts plus the blind summary.
#[derive(Debug)]
pub struct LivenessReport {
    pub verdicts: Vec<KeyVerdict>,
    pub blind: BTreeMap<String, usize>,
}

impl LivenessReport {
    /// Dead keys, in the order keys were classified.
    pub fn dead(&self) -> impl Iterator<Item = &KeyVerdict> {
        self.verdicts.iter().filter(|v| v.status == Status::Dead)
    }

    /// Suspect keys (literal present in source, but no modeled call).
    pub fn suspect(&self) -> impl Iterator<Item = &KeyVerdict> {
        self.verdicts.iter().filter(|v| v.status == Status::Suspect)
    }

    pub fn dead_count(&self) -> usize {
        self.dead().count()
    }

    pub fn suspect_count(&self) -> usize {
        self.suspect().count()
    }

    pub fn alive_count(&self) -> usize {
        self.verdicts.len() - self.dead_count() - self.suspect_count()
    }

    /// Total blind call sites across all languages.
    pub fn total_blind(&self) -> usize {
        self.blind.values().sum()
    }
}

/// The forms of a key that can satisfy liveness: the msgid plus, for plural
/// entries, the msgid_plural.
fn key_forms(key: &PoKey) -> Vec<&str> {
    let mut forms = vec![key.msgid.as_str()];
    if let Some(plural) = &key.msgid_plural {
        forms.push(plural.as_str());
    }
    forms
}

/// Classify every key in the universe against the aggregated usage.
pub fn classify(index: &PoIndex, usage: &Usage, whitelist: &HashSet<String>) -> LivenessReport {
    let verdicts = index
        .keys()
        .map(|key| KeyVerdict {
            key: key.clone(),
            status: classify_key(key, usage, whitelist),
        })
        .collect();

    LivenessReport {
        verdicts,
        blind: usage.blind.clone(),
    }
}

fn classify_key(key: &PoKey, usage: &Usage, whitelist: &HashSet<String>) -> Status {
    let forms = key_forms(key);

    if forms.iter().any(|f| whitelist.contains(*f)) {
        return Status::Alive(AliveVia::Whitelist);
    }
    if forms.iter().any(|f| usage.literals.contains(*f)) {
        return Status::Alive(AliveVia::Literal);
    }
    if usage
        .guards
        .iter()
        .any(|g| forms.iter().any(|f| g.matches(f)))
    {
        return Status::Alive(AliveVia::Guard);
    }
    if forms.iter().any(|f| usage.source_literals.contains(*f)) {
        return Status::Suspect;
    }
    Status::Dead
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

    fn plural_key(msgid: &str, plural: &str) -> PoKey {
        PoKey {
            msgctxt: None,
            msgid: msgid.to_string(),
            msgid_plural: Some(plural.to_string()),
        }
    }

    /// literal hit -> Alive/Literal; guard-only hit -> Alive/Guard; no match -> Dead.
    #[test]
    fn classifies_three_cases() {
        let index = PoIndex::from_keys([
            key("used_literal"),
            key("cf_subtype_color"),
            key("truly_dead"),
        ]);
        let mut usage = Usage::default();
        usage.literals.insert("used_literal".to_string());
        usage.guards.push(Guard::Prefix("cf_subtype_".to_string()));

        let report = classify(&index, &usage, &HashSet::new());

        let verdict = |id: &str| {
            report
                .verdicts
                .iter()
                .find(|v| v.key.msgid == id)
                .unwrap()
                .status
        };
        assert_eq!(verdict("used_literal"), Status::Alive(AliveVia::Literal));
        assert_eq!(verdict("cf_subtype_color"), Status::Alive(AliveVia::Guard));
        assert_eq!(verdict("truly_dead"), Status::Dead);
        assert_eq!(report.dead_count(), 1);
        assert_eq!(report.alive_count(), 2);
    }

    /// A key with no modeled call but whose msgid appears as a source literal is
    /// `Suspect`, not `Dead` — and a literal/guard hit still wins over Suspect.
    #[test]
    fn source_literal_only_is_suspect() {
        let index = PoIndex::from_keys([
            key("data_table_key"),
            key("never_anywhere"),
            key("also_called"),
        ]);
        let mut usage = Usage::default();
        // `data_table_key` sits as a bare literal; `also_called` is both a real
        // call literal and a source literal.
        usage
            .source_literals
            .extend(["data_table_key".to_string(), "also_called".to_string()]);
        usage.literals.insert("also_called".to_string());

        let report = classify(&index, &usage, &HashSet::new());
        let verdict = |id: &str| {
            report
                .verdicts
                .iter()
                .find(|v| v.key.msgid == id)
                .unwrap()
                .status
        };
        assert_eq!(verdict("data_table_key"), Status::Suspect);
        assert_eq!(verdict("never_anywhere"), Status::Dead);
        assert_eq!(verdict("also_called"), Status::Alive(AliveVia::Literal));
        assert_eq!(report.suspect_count(), 1);
        assert_eq!(report.dead_count(), 1);
        assert_eq!(report.alive_count(), 1);
    }

    /// A whitelisted key is Alive via Whitelist even with no code reference.
    #[test]
    fn whitelist_keeps_key_alive() {
        let index = PoIndex::from_keys([key("dynamic.from.db")]);
        let usage = Usage::default();
        let whitelist: HashSet<String> = ["dynamic.from.db".to_string()].into_iter().collect();

        let report = classify(&index, &usage, &whitelist);
        assert_eq!(
            report.verdicts[0].status,
            Status::Alive(AliveVia::Whitelist)
        );
    }

    /// A plural key is Alive if either form is referenced.
    #[test]
    fn plural_key_alive_via_plural_form() {
        let index = PoIndex::from_keys([plural_key("1 apple", "%d apples")]);
        let mut usage = Usage::default();
        usage.literals.insert("%d apples".to_string());

        let report = classify(&index, &usage, &HashSet::new());
        assert_eq!(report.verdicts[0].status, Status::Alive(AliveVia::Literal));
    }

    /// Blind sites aggregate per language and are never hidden.
    #[test]
    fn blind_summary_aggregates_per_language() {
        let mut usage = Usage::default();
        usage.add(
            "js",
            ExtractResult {
                blind: 1,
                ..Default::default()
            },
        );
        usage.add(
            "js",
            ExtractResult {
                blind: 1,
                ..Default::default()
            },
        );
        usage.add(
            "twig",
            ExtractResult {
                blind: 1,
                ..Default::default()
            },
        );

        let report = classify(&PoIndex::from_keys([]), &usage, &HashSet::new());
        assert_eq!(report.blind.get("js"), Some(&2));
        assert_eq!(report.blind.get("twig"), Some(&1));
        assert_eq!(report.total_blind(), 3);
    }
}
