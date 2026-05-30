//! Extractor adapters — per-kind matchers over each language (PLAN Addendum 2 §5).
//!
//! Contract: `extract(lang, source, calls, min_guard_len) -> ExtractResult`
//! producing `{ literals, guards, blind }`. Each `[[calls]]` entry drives a
//! matcher by `kind`:
//! - `function` → PHP `function_call_expression` / JS `call_expression` on a
//!   bare name;
//! - `method`   → PHP `member_call_expression` / JS member call, matched on name
//!   **and** a normalized receiver (`$i18n` → `i18n`, `$this->i18n` → `this.i18n`);
//! - `filter`   → Twig regex on the filter name.
//!
//! The selected `key_arg_index` argument is decomposed into [`Segment`]s: a fully
//! static argument becomes a literal; a dynamic one is routed to the guard layer;
//! a non-extractable one increments `blind`. Two concatenated string literals
//! resolve to a single literal.

use std::collections::HashSet;

use regex::Regex;
use tree_sitter::{Node, Parser};

use crate::config::{CallKind, CallSpec};
use crate::decode::{Decoded, Lang, decode_token, unescape_js, unescape_php_double};
use crate::guard::{Guard, Segment, guards_from_segments};

/// The source language of a file, selecting grammar (or the Twig regex path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLang {
    Php,
    Js,
    Jsx,
    Ts,
    Tsx,
    Twig,
}

impl SourceLang {
    /// Map a file extension to a source language.
    pub fn from_extension(ext: &str) -> Option<SourceLang> {
        match ext.to_lowercase().as_str() {
            "php" => Some(SourceLang::Php),
            "twig" => Some(SourceLang::Twig),
            "js" | "mjs" | "cjs" => Some(SourceLang::Js),
            "jsx" => Some(SourceLang::Jsx),
            "ts" | "mts" | "cts" => Some(SourceLang::Ts),
            "tsx" => Some(SourceLang::Tsx),
            _ => None,
        }
    }

    /// Coarse family label used in the per-language blind summary.
    pub fn family_label(&self) -> &'static str {
        match self {
            SourceLang::Php => "php",
            SourceLang::Twig => "twig",
            SourceLang::Js | SourceLang::Jsx | SourceLang::Ts | SourceLang::Tsx => "js",
        }
    }

    /// The tree-sitter grammar for this language (panics for Twig, which is
    /// handled by regex).
    pub fn language(&self) -> tree_sitter::Language {
        match self {
            SourceLang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            SourceLang::Js | SourceLang::Jsx => tree_sitter_javascript::LANGUAGE.into(),
            SourceLang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            SourceLang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            SourceLang::Twig => unreachable!("Twig uses the regex path, not a grammar"),
        }
    }

    fn family(&self) -> Family {
        match self {
            SourceLang::Php => Family::Php,
            SourceLang::Twig => Family::Twig,
            _ => Family::Js,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Php,
    Js,
    Twig,
}

/// What one source file contributed: resolved literal keys, keep-alive guards,
/// the count of blind (unresolvable) call sites, and every static string literal
/// seen anywhere in the file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtractResult {
    pub literals: HashSet<String>,
    pub guards: Vec<Guard>,
    pub blind: usize,
    /// Every fully-static string literal seen *anywhere* in the file, not only in
    /// translation calls. Feeds the `Suspect` tier: a dead-looking key that still
    /// appears verbatim as a source literal is likely dispatched dynamically
    /// (data tables, enums, factory lookups) — flag for review, don't bury it.
    pub source_literals: HashSet<String>,
}

impl ExtractResult {
    fn record(&mut self, segments: Vec<Segment>, min_guard_len: usize) {
        let has_hole = segments.iter().any(|s| matches!(s, Segment::Hole));
        if !has_hole {
            // Fully static: join the fragments into one literal key.
            let literal: String = segments
                .iter()
                .filter_map(|s| match s {
                    Segment::Static(t) => Some(t.as_str()),
                    Segment::Hole => None,
                })
                .collect();
            self.literals.insert(literal);
        } else {
            let ext = guards_from_segments(&segments, min_guard_len);
            self.guards.extend(ext.guards);
            if ext.blind {
                self.blind += 1;
            }
        }
    }

    /// Record a string literal seen in the source (any context). Only fully
    /// static, non-empty literals are kept — a dynamic fragment is useless for an
    /// exact-msgid match and would only add noise.
    fn record_source_literal(&mut self, segments: &[Segment]) {
        if !segments.iter().all(|s| matches!(s, Segment::Static(_))) {
            return;
        }
        let literal: String = segments
            .iter()
            .filter_map(|s| match s {
                Segment::Static(t) => Some(t.as_str()),
                Segment::Hole => None,
            })
            .collect();
        if !literal.is_empty() {
            self.source_literals.insert(literal);
        }
    }
}

/// A reusable set of tree-sitter parsers, one lazily-created slot per grammar.
///
/// `tree_sitter::Parser` is **not `Sync`** and costly to recreate per file, so a
/// pool is held thread-locally by the scan workers and reused across files
/// (PLAN Addendum 2 §6).
#[derive(Default)]
pub struct ParserPool {
    php: Option<Parser>,
    js: Option<Parser>,
    ts: Option<Parser>,
    tsx: Option<Parser>,
}

impl ParserPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (lazily creating) the parser for a language, with its grammar set.
    /// Returns `None` for Twig (regex path) or on grammar load failure.
    fn get(&mut self, lang: SourceLang) -> Option<&mut Parser> {
        let slot = match lang {
            SourceLang::Php => &mut self.php,
            SourceLang::Js | SourceLang::Jsx => &mut self.js,
            SourceLang::Ts => &mut self.ts,
            SourceLang::Tsx => &mut self.tsx,
            SourceLang::Twig => return None,
        };
        if slot.is_none() {
            let mut parser = Parser::new();
            if parser.set_language(&lang.language()).is_err() {
                return None;
            }
            *slot = Some(parser);
        }
        slot.as_mut()
    }
}

/// Extract translation keys from one source file, creating a throwaway parser.
/// Convenient for one-off use and tests; the scan path uses [`extract_with_pool`].
pub fn extract(
    lang: SourceLang,
    source: &str,
    calls: &[CallSpec],
    min_guard_len: usize,
) -> ExtractResult {
    let mut pool = ParserPool::new();
    extract_with_pool(&mut pool, lang, source, calls, min_guard_len)
}

/// Extract translation keys reusing parsers from `pool`.
pub fn extract_with_pool(
    pool: &mut ParserPool,
    lang: SourceLang,
    source: &str,
    calls: &[CallSpec],
    min_guard_len: usize,
) -> ExtractResult {
    // Note: we do *not* early-return when no call spec applies. Source-literal
    // collection (the `Suspect` tier) must see every file, including pure data
    // files that hold no translation calls at all (currency tables, enums, …).
    let applicable: Vec<&CallSpec> = calls.iter().filter(|c| call_applies(c, lang)).collect();
    match lang.family() {
        Family::Twig => extract_twig(source, &applicable),
        _ => match pool.get(lang) {
            Some(parser) => extract_ast_with(parser, lang, source, &applicable, min_guard_len),
            None => ExtractResult::default(),
        },
    }
}

/// Whether a call spec's `lang` applies to this source language.
fn call_applies(call: &CallSpec, lang: SourceLang) -> bool {
    let l = call.lang.to_lowercase();
    match lang {
        SourceLang::Php => l == "php",
        SourceLang::Twig => l == "twig",
        SourceLang::Js | SourceLang::Jsx | SourceLang::Ts | SourceLang::Tsx => {
            matches!(l.as_str(), "js" | "jsx" | "ts" | "tsx" | "javascript" | "typescript")
        }
    }
}

// ---------------------------------------------------------------------------
// AST path (PHP / JS / TS / TSX)
// ---------------------------------------------------------------------------

/// Parse with an already-configured parser (its grammar set to `lang`) and walk.
fn extract_ast_with(
    parser: &mut Parser,
    lang: SourceLang,
    source: &str,
    calls: &[&CallSpec],
    min_guard_len: usize,
) -> ExtractResult {
    let Some(tree) = parser.parse(source, None) else {
        return ExtractResult::default();
    };

    let mut res = ExtractResult::default();
    let family = lang.family();
    for_each_node(tree.root_node(), |node| {
        match family {
            Family::Php => try_php_call(node, source, calls, min_guard_len, &mut res),
            Family::Js => {
                try_js_call(node, source, calls, min_guard_len, &mut res);
                try_js_index(node, source, calls, min_guard_len, &mut res);
            }
            Family::Twig => {}
        }
        collect_ast_literal(node, family, source, &mut res);
    });
    res
}

/// Collect a fully-static string literal node into `source_literals`, regardless
/// of whether it sits inside a translation call (the `Suspect` tier).
fn collect_ast_literal(node: Node, family: Family, src: &str, res: &mut ExtractResult) {
    let segments = match family {
        Family::Php => match node.kind() {
            "string" | "encapsed_string" => php_segments(node, src),
            _ => return,
        },
        Family::Js => match node.kind() {
            "string" | "template_string" => js_segments(node, src),
            _ => return,
        },
        Family::Twig => return,
    };
    res.record_source_literal(&segments);
}

/// Iterative pre-order walk over every node.
fn for_each_node<F: FnMut(Node)>(root: Node, mut f: F) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        f(node);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn receiver_ok(spec: &CallSpec, receiver: &str) -> bool {
    match &spec.receiver {
        Some(list) => list.iter().any(|r| r == receiver),
        None => true,
    }
}

// --- PHP -------------------------------------------------------------------

fn try_php_call(
    node: Node,
    src: &str,
    calls: &[&CallSpec],
    min_guard_len: usize,
    res: &mut ExtractResult,
) {
    match node.kind() {
        "function_call_expression" => {
            let Some(func) = node.child_by_field_name("function") else {
                return;
            };
            if func.kind() != "name" {
                return;
            }
            let name = text(func, src);
            if let Some(spec) = calls
                .iter()
                .find(|c| c.kind == CallKind::Function && c.name == name)
            {
                record_php_key_arg(node, spec, src, min_guard_len, res);
            }
        }
        "member_call_expression" => {
            let (Some(name_node), Some(obj)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("object"),
            ) else {
                return;
            };
            let mname = text(name_node, src);
            let receiver = normalize_php_receiver(obj, src);
            if let Some(spec) = calls.iter().find(|c| {
                c.kind == CallKind::Method && c.name == mname && receiver_ok(c, &receiver)
            }) {
                record_php_key_arg(node, spec, src, min_guard_len, res);
            }
        }
        "scoped_call_expression" => {
            let (Some(name_node), Some(scope)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("scope"),
            ) else {
                return;
            };
            let mname = text(name_node, src);
            let receiver = text(scope, src).trim_start_matches('$').to_string();
            if let Some(spec) = calls.iter().find(|c| {
                c.kind == CallKind::Method && c.name == mname && receiver_ok(c, &receiver)
            }) {
                record_php_key_arg(node, spec, src, min_guard_len, res);
            }
        }
        _ => {}
    }
}

fn record_php_key_arg(
    call: Node,
    spec: &CallSpec,
    src: &str,
    min_guard_len: usize,
    res: &mut ExtractResult,
) {
    let Some(args) = call.child_by_field_name("arguments") else {
        return;
    };
    let values = php_arg_values(args);
    if let Some(&arg) = values.get(spec.key_arg_index) {
        let segments = php_segments(arg, src);
        res.record(segments, min_guard_len);
    }
}

/// Collect PHP argument *value* nodes (unwrapping each `argument`).
fn php_arg_values(args: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut cursor = args.walk();
    for child in args.named_children(&mut cursor) {
        if child.kind() == "argument" {
            let count = child.named_child_count();
            if count > 0 {
                if let Some(value) = child.named_child(count as u32 - 1) {
                    out.push(value);
                }
            }
        }
    }
    out
}

fn normalize_php_receiver(node: Node, src: &str) -> String {
    match node.kind() {
        "variable_name" => text(node, src).trim_start_matches('$').to_string(),
        "name" => text(node, src).to_string(),
        "member_access_expression" => {
            match (
                node.child_by_field_name("object"),
                node.child_by_field_name("name"),
            ) {
                (Some(obj), Some(name)) => {
                    format!("{}.{}", normalize_php_receiver(obj, src), text(name, src))
                }
                _ => text(node, src).trim_start_matches('$').to_string(),
            }
        }
        // The receiver is itself a call returning the i18n object: a factory like
        // `Container::get_i18n()->get(…)`, `get_i18n()->get(…)`, or
        // `$x->getI18n()->get(…)`. Normalize to the callee name + `()` so the
        // config anchors on the factory (`get_i18n()`), not the class it hangs
        // off — by far the most common PHP convention in real codebases.
        "function_call_expression" | "scoped_call_expression" | "member_call_expression" => {
            match node.child_by_field_name(if node.kind() == "function_call_expression" {
                "function"
            } else {
                "name"
            }) {
                Some(callee) => format!("{}()", text(callee, src)),
                None => text(node, src).trim_start_matches('$').to_string(),
            }
        }
        _ => text(node, src).trim_start_matches('$').to_string(),
    }
}

fn php_segments(node: Node, src: &str) -> Vec<Segment> {
    match node.kind() {
        "string" => match decode_token(Lang::Php, text(node, src)) {
            Decoded::Literal(s) => vec![Segment::Static(s)],
            Decoded::Dynamic => vec![Segment::Hole],
        },
        "encapsed_string" => {
            let mut segs = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "string_content" => segs.push(Segment::Static(text(child, src).to_string())),
                    "escape_sequence" => {
                        segs.push(Segment::Static(unescape_php_double(text(child, src))))
                    }
                    _ => segs.push(Segment::Hole),
                }
            }
            if segs.is_empty() {
                vec![Segment::Static(String::new())]
            } else {
                segs
            }
        }
        "binary_expression" if binary_operator(node, src).as_deref() == Some(".") => {
            concat_segments(node, src, php_segments)
        }
        _ => vec![Segment::Hole],
    }
}

// --- JS / TS ---------------------------------------------------------------

fn try_js_call(
    node: Node,
    src: &str,
    calls: &[&CallSpec],
    min_guard_len: usize,
    res: &mut ExtractResult,
) {
    if node.kind() != "call_expression" {
        return;
    }
    let Some(func) = node.child_by_field_name("function") else {
        return;
    };
    match func.kind() {
        "identifier" => {
            let name = text(func, src);
            if let Some(spec) = calls
                .iter()
                .find(|c| c.kind == CallKind::Function && c.name == name)
            {
                record_js_key_arg(node, spec, src, min_guard_len, res);
            }
        }
        "member_expression" => {
            let (Some(prop), Some(obj)) = (
                func.child_by_field_name("property"),
                func.child_by_field_name("object"),
            ) else {
                return;
            };
            let name = text(prop, src);
            let receiver = normalize_js_receiver(obj, src);
            if let Some(spec) = calls.iter().find(|c| {
                c.kind == CallKind::Method && c.name == name && receiver_ok(c, &receiver)
            }) {
                record_js_key_arg(node, spec, src, min_guard_len, res);
            }
        }
        _ => {}
    }
}

/// Index access `obj['key']` (JS `subscript_expression`). Matched when the
/// object normalizes to the configured `name` (`locale`, `this.locale`, …); the
/// subscript is decoded by the same literal/guard/blind path as a call argument.
fn try_js_index(
    node: Node,
    src: &str,
    calls: &[&CallSpec],
    min_guard_len: usize,
    res: &mut ExtractResult,
) {
    if node.kind() != "subscript_expression" {
        return;
    }
    let (Some(obj), Some(index)) = (
        node.child_by_field_name("object"),
        node.child_by_field_name("index"),
    ) else {
        return;
    };
    let obj_name = normalize_js_receiver(obj, src);
    if calls
        .iter()
        .any(|c| c.kind == CallKind::Index && c.name == obj_name)
    {
        let segments = js_segments(index, src);
        res.record(segments, min_guard_len);
    }
}

fn record_js_key_arg(
    call: Node,
    spec: &CallSpec,
    src: &str,
    min_guard_len: usize,
    res: &mut ExtractResult,
) {
    let Some(args) = call.child_by_field_name("arguments") else {
        return;
    };
    let mut values = Vec::new();
    let mut cursor = args.walk();
    for child in args.named_children(&mut cursor) {
        if child.kind() != "comment" {
            values.push(child);
        }
    }
    if let Some(&arg) = values.get(spec.key_arg_index) {
        let segments = js_segments(arg, src);
        res.record(segments, min_guard_len);
    }
}

fn normalize_js_receiver(node: Node, src: &str) -> String {
    match node.kind() {
        "identifier" | "property_identifier" => text(node, src).to_string(),
        "this" => "this".to_string(),
        "member_expression" => {
            match (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            ) {
                (Some(obj), Some(prop)) => {
                    format!("{}.{}", normalize_js_receiver(obj, src), text(prop, src))
                }
                _ => text(node, src).to_string(),
            }
        }
        _ => text(node, src).to_string(),
    }
}

fn js_segments(node: Node, src: &str) -> Vec<Segment> {
    match node.kind() {
        "string" => match decode_token(Lang::Js, text(node, src)) {
            Decoded::Literal(s) => vec![Segment::Static(s)],
            Decoded::Dynamic => vec![Segment::Hole],
        },
        "template_string" => {
            let mut segs = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "template_substitution" => segs.push(Segment::Hole),
                    "escape_sequence" => {
                        segs.push(Segment::Static(unescape_js(text(child, src))))
                    }
                    // string_fragment and anything else: raw static text.
                    _ => segs.push(Segment::Static(text(child, src).to_string())),
                }
            }
            if segs.is_empty() {
                vec![Segment::Static(String::new())]
            } else {
                segs
            }
        }
        "binary_expression" if binary_operator(node, src).as_deref() == Some("+") => {
            concat_segments(node, src, js_segments)
        }
        _ => vec![Segment::Hole],
    }
}

// --- shared AST helpers ----------------------------------------------------

/// The operator token of a binary expression (first unnamed child).
fn binary_operator(node: Node, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            return Some(text(child, src).to_string());
        }
    }
    None
}

/// Flatten a string concatenation into the segments of its two operands.
fn concat_segments(
    node: Node,
    src: &str,
    f: fn(Node, &str) -> Vec<Segment>,
) -> Vec<Segment> {
    let mut segs = Vec::new();
    if let Some(left) = node.child_by_field_name("left") {
        segs.extend(f(left, src));
    }
    if let Some(right) = node.child_by_field_name("right") {
        segs.extend(f(right, src));
    }
    segs
}

// ---------------------------------------------------------------------------
// Twig path (regex)
// ---------------------------------------------------------------------------

fn extract_twig(source: &str, calls: &[&CallSpec]) -> ExtractResult {
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
        let lit_re =
            Regex::new(&format!(r#"['"]([^'"]*)['"]\s*\|\s*{name}\b"#)).expect("valid literal regex");

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CallKind;

    fn func(lang: &str, name: &str) -> CallSpec {
        CallSpec {
            lang: lang.to_string(),
            kind: CallKind::Function,
            name: name.to_string(),
            receiver: None,
            key_arg_index: 0,
        }
    }

    fn method(lang: &str, name: &str, receiver: &[&str]) -> CallSpec {
        CallSpec {
            lang: lang.to_string(),
            kind: CallKind::Method,
            name: name.to_string(),
            receiver: Some(receiver.iter().map(|s| s.to_string()).collect()),
            key_arg_index: 0,
        }
    }

    fn filter(name: &str) -> CallSpec {
        CallSpec {
            lang: "twig".to_string(),
            kind: CallKind::Filter,
            name: name.to_string(),
            receiver: None,
            key_arg_index: 0,
        }
    }

    fn index(lang: &str, name: &str) -> CallSpec {
        CallSpec {
            lang: lang.to_string(),
            kind: CallKind::Index,
            name: name.to_string(),
            receiver: None,
            key_arg_index: 0,
        }
    }

    fn lit(res: &ExtractResult) -> Vec<String> {
        let mut v: Vec<String> = res.literals.iter().cloned().collect();
        v.sort();
        v
    }

    #[test]
    fn php_function_matcher() {
        let src = "<?php i18n('hello'); notI18n('skip'); ?>";
        let res = extract(SourceLang::Php, src, &[func("php", "i18n")], 3);
        assert_eq!(lit(&res), vec!["hello".to_string()]);
    }

    #[test]
    fn php_method_matcher_with_receiver() {
        let src = "<?php $i18n->get('k1'); $this->i18n->get('k2'); $other->get('nope'); ?>";
        let res = extract(SourceLang::Php, src, &[method("php", "get", &["i18n", "this.i18n"])], 3);
        assert_eq!(lit(&res), vec!["k1".to_string(), "k2".to_string()]);
    }

    /// A factory-call receiver — `Container::get_i18n()->get('k')` and the free
    /// form `get_i18n()->get('k')` — is normalized to the token `get_i18n()`.
    #[test]
    fn php_factory_call_receiver() {
        let src = "<?php Container::get_i18n()->get('k1'); get_i18n()->get('k2'); \
                   $other->build()->get('nope'); ?>";
        let res = extract(SourceLang::Php, src, &[method("php", "get", &["get_i18n()"])], 3);
        assert_eq!(lit(&res), vec!["k1".to_string(), "k2".to_string()]);
    }

    #[test]
    fn php_concatenation_of_two_literals() {
        let src = "<?php i18n('Hello, ' . 'World'); ?>";
        let res = extract(SourceLang::Php, src, &[func("php", "i18n")], 3);
        assert_eq!(lit(&res), vec!["Hello, World".to_string()]);
    }

    #[test]
    fn php_concatenation_with_variable_yields_prefix_guard() {
        let src = "<?php i18n('Hello, ' . $x); ?>";
        let res = extract(SourceLang::Php, src, &[func("php", "i18n")], 3);
        assert!(res.literals.is_empty());
        assert_eq!(res.guards, vec![Guard::Prefix("Hello, ".to_string())]);
        assert_eq!(res.blind, 0);
    }

    #[test]
    fn php_double_quote_interpolation_is_dynamic() {
        // "role_$x" -> Prefix("role_")
        let src = "<?php i18n(\"role_$x\"); ?>";
        let res = extract(SourceLang::Php, src, &[func("php", "i18n")], 3);
        assert!(res.literals.is_empty());
        assert_eq!(res.guards, vec![Guard::Prefix("role_".to_string())]);
    }

    #[test]
    fn js_function_and_template_guard() {
        let src = "i18n('k1'); i18n(`cf_${sub}`); i18n($x);";
        let res = extract(SourceLang::Js, src, &[func("js", "i18n")], 3);
        assert_eq!(lit(&res), vec!["k1".to_string()]);
        assert_eq!(res.guards, vec![Guard::Prefix("cf_".to_string())]);
        assert_eq!(res.blind, 1, "i18n($x) is a blind site");
    }

    #[test]
    fn js_short_fragment_template_is_blind_not_alive() {
        let src = "i18n(`${a}_${b}`);";
        let res = extract(SourceLang::Js, src, &[func("js", "i18n")], 3);
        assert!(res.guards.is_empty());
        assert_eq!(res.blind, 1);
    }

    #[test]
    fn js_method_matcher() {
        let src = "i18n.t('k'); other.t('nope');";
        let res = extract(SourceLang::Js, src, &[method("js", "t", &["i18n"])], 3);
        assert_eq!(lit(&res), vec!["k".to_string()]);
    }

    #[test]
    fn js_index_matcher_literal_dynamic_and_mismatch() {
        // locale['Delete email'] -> literal; locale[varKey] -> blind;
        // other['x'] -> ignored (object name mismatch).
        let src = "locale['Delete email']; locale[varKey]; other['x'];";
        let res = extract(SourceLang::Js, src, &[index("js", "locale")], 3);
        assert_eq!(lit(&res), vec!["Delete email".to_string()]);
        assert_eq!(res.blind, 1, "locale[varKey] is a blind site");
    }

    /// Index access also routes a dynamic-but-prefixed subscript to a guard,
    /// and matches a member object via the normalized receiver (`this.locale`).
    #[test]
    fn js_index_matcher_guard_and_member_object() {
        let src = "this.locale[`cf_subtype_${k}`];";
        let res = extract(SourceLang::Js, src, &[index("js", "this.locale")], 3);
        assert!(res.literals.is_empty());
        assert_eq!(res.guards, vec![Guard::Prefix("cf_subtype_".to_string())]);
    }

    #[test]
    fn twig_filter_literal_and_blind() {
        let src = "{{ 'k'|i18n }} and {{ var|i18n }}";
        let res = extract(SourceLang::Twig, src, &[filter("i18n")], 3);
        assert_eq!(lit(&res), vec!["k".to_string()]);
        assert_eq!(res.blind, 1, "{{ var|i18n }} is blind");
    }

    /// HTML-aware PHP grammar parses mixed markup without panic.
    #[test]
    fn php_mixed_html_parses() {
        let src = "<html><body><?php i18n('mixed'); ?></body></html>";
        let res = extract(SourceLang::Php, src, &[func("php", "i18n")], 3);
        assert_eq!(lit(&res), vec!["mixed".to_string()]);
    }

    /// TSX is handled by the TSX grammar.
    #[test]
    fn tsx_grammar_extracts() {
        let src = "const e = <div>{i18n('tsx_key')}</div>;";
        let res = extract(SourceLang::Tsx, src, &[func("ts", "i18n")], 3);
        assert_eq!(lit(&res), vec!["tsx_key".to_string()]);
    }

    /// Source-literal collection (Suspect tier) sees *every* static string,
    /// including bare data-table values with no translation call, and even when
    /// no call spec applies to the file. Dynamic strings are excluded.
    #[test]
    fn collects_source_literals_outside_calls() {
        let src = "<?php const X = 'industry_retail_ecommerce'; \
                   $a = [RPI::T => 'Count of income calls']; \
                   $b = \"role_$x\"; ?>";
        // No applicable calls at all — collection must still run.
        let res = extract(SourceLang::Php, src, &[], 3);
        assert!(res.source_literals.contains("industry_retail_ecommerce"));
        assert!(res.source_literals.contains("Count of income calls"));
        // Interpolated string is not a static literal.
        assert!(!res.source_literals.iter().any(|s| s.contains("role_")));
    }

    /// A key passed to a real call is in `literals`; the same scan still records
    /// it among `source_literals` (harmless — liveness checks literals first).
    #[test]
    fn js_source_literals_include_template_and_plain() {
        let src = "i18n('called_key'); const m = {'bare_key': 1};";
        let res = extract(SourceLang::Js, src, &[func("js", "i18n")], 3);
        assert!(res.source_literals.contains("called_key"));
        assert!(res.source_literals.contains("bare_key"));
    }
}
