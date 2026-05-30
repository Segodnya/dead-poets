//! Twig adapter — regex, not a tree. Twig has no tree-sitter grammar here, so it
//! is a separate path rather than an `AstMatcher`: a literal `'key'|filter` is a
//! resolved key, any other use of the filter is a blind site, and every quoted
//! string feeds the Suspect tier.

use regex::Regex;

use crate::config::{CallKind, CallSpec};

use super::ExtractResult;

pub(super) fn extract_twig(source: &str, calls: &[&CallSpec]) -> ExtractResult {
    let mut res = ExtractResult::default();

    // Source-literal collection (Suspect tier): every quoted string in the
    // template, independent of any filter. Interpolated strings (`#{…}`) are not
    // static and are skipped.
    let str_re = Regex::new(r#"'([^'\n]*)'|"([^"\n]*)""#).expect("valid string regex");
    for cap in str_re.captures_iter(source) {
        if let Some(m) = cap.get(1).or_else(|| cap.get(2)) {
            let s = m.as_str();
            if !s.is_empty() && !s.contains("#{") {
                res.source_literals.insert(s.to_string());
            }
        }
    }

    for call in calls.iter().filter(|c| c.kind == CallKind::Filter) {
        let name = regex::escape(&call.name);
        let total_re = Regex::new(&format!(r"\|\s*{name}\b")).expect("valid total regex");
        let lit_re = Regex::new(&format!(r#"['"]([^'"]*)['"]\s*\|\s*{name}\b"#))
            .expect("valid literal regex");

        let total = total_re.find_iter(source).count();
        let mut kept = 0;
        for cap in lit_re.captures_iter(source) {
            let key = &cap[1];
            // Twig string interpolation (`#{…}`) can't be resolved statically.
            if key.contains("#{") {
                continue;
            }
            res.literals.insert(key.to_string());
            kept += 1;
        }
        // Every use we couldn't resolve to a literal is a blind spot.
        res.blind += total.saturating_sub(kept);
    }
    res
}
