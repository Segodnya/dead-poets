//! Guard layer — the central correctness invariant (PLAN Addendum 2 §1).
//!
//! A dynamic key argument (template string, concatenation, interpolated
//! double-quote) is modelled as an ordered list of [`Segment`]s: decoded static
//! text interleaved with [`Segment::Hole`]s (the `${…}`/`$var`/concatenated
//! variable parts). From the **maximal** static fragments we emit guards:
//!
//! - a fragment at the very start → [`Guard::Prefix`];
//! - at the very end → [`Guard::Suffix`];
//! - interior → [`Guard::Contains`].
//!
//! **Invariant:** a guard is never empty and never shorter than `min_guard_len`.
//! A fragment below the threshold does not create a guard. If a call site yields
//! no qualifying fragment it is a **blind spot** (`blind = true`) — never
//! silently dropped. This kills the landmine where `` `${a}_${b}` `` (only a
//! one-char `"_"` fragment) would otherwise mark the entire catalog Alive.
//!
//! Matching is canonical-substring/prefix/suffix against the **decoded** msgid.

/// One piece of a dynamic key argument. `Static` holds already-decoded
/// (canonical) text; `Hole` is a dynamic, statically-unknown segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Static(String),
    Hole,
}

/// A keep-it-alive guard derived from a static fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    Prefix(String),
    Suffix(String),
    Contains(String),
}

impl Guard {
    /// Whether this guard matches a decoded msgid.
    pub fn matches(&self, msgid: &str) -> bool {
        match self {
            Guard::Prefix(p) => msgid.starts_with(p.as_str()),
            Guard::Suffix(s) => msgid.ends_with(s.as_str()),
            Guard::Contains(c) => msgid.contains(c.as_str()),
        }
    }
}

/// The guards (possibly none) and blind-spot flag for one dynamic call site.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuardExtraction {
    pub guards: Vec<Guard>,
    /// `true` when no fragment qualified — the site could not be checked at all.
    pub blind: bool,
}

/// Build guards from a dynamic argument's segments, enforcing the
/// `min_guard_len` invariant.
pub fn guards_from_segments(segments: &[Segment], min_guard_len: usize) -> GuardExtraction {
    let last = segments.len().saturating_sub(1);
    let mut guards = Vec::new();

    for (i, segment) in segments.iter().enumerate() {
        let Segment::Static(text) = segment else {
            continue;
        };
        // Drop empty/too-short fragments — never emit a trivial guard.
        if text.chars().count() < min_guard_len {
            continue;
        }
        let at_start = i == 0;
        let at_end = i == last;
        let guard = match (at_start, at_end) {
            // Spans the whole string with no hole: should not reach the guard
            // path (it would be a plain literal). Treat defensively as Contains.
            (true, true) => Guard::Contains(text.clone()),
            (true, false) => Guard::Prefix(text.clone()),
            (false, true) => Guard::Suffix(text.clone()),
            (false, false) => Guard::Contains(text.clone()),
        };
        guards.push(guard);
    }

    let blind = guards.is_empty();
    GuardExtraction { guards, blind }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Segment {
        Segment::Static(text.to_string())
    }

    /// `` `cf_subtype_${x}` `` -> Prefix("cf_subtype_").
    #[test]
    fn leading_fragment_becomes_prefix() {
        let segs = [s("cf_subtype_"), Segment::Hole];
        let ext = guards_from_segments(&segs, 3);
        assert_eq!(ext.guards, vec![Guard::Prefix("cf_subtype_".to_string())]);
        assert!(!ext.blind);
    }

    /// `` `${x}_delete_confirm_1` `` -> Suffix("_delete_confirm_1").
    #[test]
    fn trailing_fragment_becomes_suffix() {
        let segs = [Segment::Hole, s("_delete_confirm_1")];
        let ext = guards_from_segments(&segs, 3);
        assert_eq!(ext.guards, vec![Guard::Suffix("_delete_confirm_1".to_string())]);
        assert!(!ext.blind);
    }

    /// Interior fragment -> Contains.
    #[test]
    fn interior_fragment_becomes_contains() {
        let segs = [Segment::Hole, s("_middle_"), Segment::Hole];
        let ext = guards_from_segments(&segs, 3);
        assert_eq!(ext.guards, vec![Guard::Contains("_middle_".to_string())]);
    }

    /// `` `${a}_${b}` `` — only a one-char `"_"` fragment -> NO guard, blind++.
    /// Must never mark the whole catalog Alive.
    #[test]
    fn short_fragment_yields_no_guard_and_is_blind() {
        let segs = [Segment::Hole, s("_"), Segment::Hole];
        let ext = guards_from_segments(&segs, 3);
        assert!(ext.guards.is_empty());
        assert!(ext.blind);
    }

    /// `i18n($x)` — no static part at all -> blind.
    #[test]
    fn pure_variable_is_blind() {
        let segs = [Segment::Hole];
        let ext = guards_from_segments(&segs, 3);
        assert!(ext.guards.is_empty());
        assert!(ext.blind);
    }

    /// Threshold is inclusive: a fragment exactly `min_guard_len` long is kept.
    #[test]
    fn fragment_at_threshold_is_kept() {
        let segs = [s("cf_"), Segment::Hole];
        let ext = guards_from_segments(&segs, 3);
        assert_eq!(ext.guards, vec![Guard::Prefix("cf_".to_string())]);
    }

    /// Guard matching against decoded msgids.
    #[test]
    fn guard_matching() {
        let prefix = Guard::Prefix("cf_subtype_".to_string());
        assert!(prefix.matches("cf_subtype_foo"));
        assert!(!prefix.matches("other_key"));

        let suffix = Guard::Suffix("_delete_confirm_1".to_string());
        assert!(suffix.matches("entity_delete_confirm_1"));
        assert!(!suffix.matches("entity_delete_confirm_2"));

        let contains = Guard::Contains("_middle_".to_string());
        assert!(contains.matches("a_middle_b"));
        assert!(!contains.matches("a_b"));
    }
}
