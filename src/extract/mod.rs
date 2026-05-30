//! Extractor — per-language adapters behind one invariant driver.
//!
//! Contract: `extract(lang, source, calls, min_guard_len) -> ExtractResult`
//! producing `{ literals, guards, blind, source_literals }`. The work splits in
//! two:
//!
//! - **what varies per grammar** lives behind the [`AstMatcher`] seam — one
//!   adapter per family ([`php::Php`], [`js::Js`]): classify a node as a call
//!   site, decode a value node into [`Segment`]s, and recognise a string literal.
//! - **what is invariant** lives here, written once: [`match_spec`] resolves a
//!   classified [`CallShape`] against the configured `[[calls]]`, the driver
//!   selects `key_arg_index`, and [`ExtractResult::record`] routes a fully-static
//!   argument to a literal and a dynamic one to the guard layer (or a blind site).
//!
//! Twig has no tree — it is a separate regex adapter ([`twig::extract_twig`]),
//! not an `AstMatcher`. Two concatenated string literals resolve to one literal.

mod js;
mod php;
mod twig;

use std::collections::HashSet;

use tree_sitter::{Node, Parser};

use crate::config::{CallKind, CallSpec};
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
/// pool is held thread-locally by the scan workers and reused across files.
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
    match lang {
        SourceLang::Twig => twig::extract_twig(source, &applicable),
        SourceLang::Php => run_ast::<php::Php>(pool, lang, source, &applicable, min_guard_len),
        SourceLang::Js | SourceLang::Jsx | SourceLang::Ts | SourceLang::Tsx => {
            run_ast::<js::Js>(pool, lang, source, &applicable, min_guard_len)
        }
    }
}

/// Whether a call spec's `lang` applies to this source language.
fn call_applies(call: &CallSpec, lang: SourceLang) -> bool {
    let l = call.lang.to_lowercase();
    match lang {
        SourceLang::Php => l == "php",
        SourceLang::Twig => l == "twig",
        SourceLang::Js | SourceLang::Jsx | SourceLang::Ts | SourceLang::Tsx => {
            matches!(
                l.as_str(),
                "js" | "jsx" | "ts" | "tsx" | "javascript" | "typescript"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// The AST seam (PHP / JS / TS / TSX)
// ---------------------------------------------------------------------------

/// A classified call site: enough for the driver to match a `[[calls]]` spec and
/// locate the key argument. Holds tree nodes (borrowed for the walk) plus the
/// already-extracted name/receiver — the adapter does the grammar-specific
/// decoding, the driver does the invariant matching.
pub(crate) enum CallShape<'a> {
    /// Free function: `i18n('key')`.
    Function { name: String, args: Vec<Node<'a>> },
    /// Method with a normalized receiver: `$i18n->get('key')`.
    Method {
        name: String,
        receiver: String,
        args: Vec<Node<'a>>,
    },
    /// Index access: `locale['key']`. `object` is the normalized indexed object;
    /// `key` is the subscript node.
    Index { object: String, key: Node<'a> },
}

impl<'a> CallShape<'a> {
    /// The key-argument node this site carries: the `key_arg_index`-th positional
    /// argument for a call, or the subscript for an index access.
    fn key_arg(&self, spec: &CallSpec) -> Option<Node<'a>> {
        match self {
            CallShape::Function { args, .. } | CallShape::Method { args, .. } => {
                args.get(spec.key_arg_index).copied()
            }
            CallShape::Index { key, .. } => Some(*key),
        }
    }
}

/// A per-language AST adapter: the small, grammar-specific surface the driver
/// drives. Each method is a pure node → value mapping, so it is testable in
/// isolation against a parsed snippet (see the per-adapter tests).
pub(crate) trait AstMatcher {
    /// Classify a node as a translation call site, if it is one. The returned
    /// shape borrows the tree (`'a`), not `src`.
    fn classify<'a>(node: Node<'a>, src: &str) -> Option<CallShape<'a>>;
    /// Decode a value node into key segments (static text interleaved with holes).
    fn segments(node: Node, src: &str) -> Vec<Segment>;
    /// If this node is a string literal, its decoded segments — fed to the
    /// `Suspect` tier's source-literal collection regardless of context.
    fn literal(node: Node, src: &str) -> Option<Vec<Segment>>;
}

/// Resolve a classified call site against the applicable `[[calls]]` specs. This
/// predicate is the single invariant that used to be copy-pasted per language.
fn match_spec<'c>(calls: &[&'c CallSpec], shape: &CallShape) -> Option<&'c CallSpec> {
    calls.iter().copied().find(|c| match shape {
        CallShape::Function { name, .. } => c.kind == CallKind::Function && &c.name == name,
        CallShape::Method { name, receiver, .. } => {
            c.kind == CallKind::Method && &c.name == name && receiver_ok(c, receiver)
        }
        CallShape::Index { object, .. } => c.kind == CallKind::Index && &c.name == object,
    })
}

/// Fetch the pool's parser for `lang` and run the driver with adapter `M`.
fn run_ast<M: AstMatcher>(
    pool: &mut ParserPool,
    lang: SourceLang,
    source: &str,
    calls: &[&CallSpec],
    min_guard_len: usize,
) -> ExtractResult {
    match pool.get(lang) {
        Some(parser) => extract_ast::<M>(parser, source, calls, min_guard_len),
        None => ExtractResult::default(),
    }
}

/// Parse and walk every node once, driving adapter `M`: match-and-record key
/// arguments, and collect source literals for the Suspect tier.
fn extract_ast<M: AstMatcher>(
    parser: &mut Parser,
    source: &str,
    calls: &[&CallSpec],
    min_guard_len: usize,
) -> ExtractResult {
    let Some(tree) = parser.parse(source, None) else {
        return ExtractResult::default();
    };

    let mut res = ExtractResult::default();
    for_each_node(tree.root_node(), |node| {
        if let Some(shape) = M::classify(node, source)
            && let Some(spec) = match_spec(calls, &shape)
            && let Some(arg) = shape.key_arg(spec)
        {
            res.record(M::segments(arg, source), min_guard_len);
        }
        if let Some(segments) = M::literal(node, source) {
            res.record_source_literal(&segments);
        }
    });
    res
}

// --- shared AST helpers ----------------------------------------------------

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
fn concat_segments(node: Node, src: &str, f: fn(Node, &str) -> Vec<Segment>) -> Vec<Segment> {
    let mut segs = Vec::new();
    if let Some(left) = node.child_by_field_name("left") {
        segs.extend(f(left, src));
    }
    if let Some(right) = node.child_by_field_name("right") {
        segs.extend(f(right, src));
    }
    segs
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
        let res = extract(
            SourceLang::Php,
            src,
            &[method("php", "get", &["i18n", "this.i18n"])],
            3,
        );
        assert_eq!(lit(&res), vec!["k1".to_string(), "k2".to_string()]);
    }

    /// A factory-call receiver — `Container::get_i18n()->get('k')` and the free
    /// form `get_i18n()->get('k')` — is normalized to the token `get_i18n()`.
    #[test]
    fn php_factory_call_receiver() {
        let src = "<?php Container::get_i18n()->get('k1'); get_i18n()->get('k2'); \
                   $other->build()->get('nope'); ?>";
        let res = extract(
            SourceLang::Php,
            src,
            &[method("php", "get", &["get_i18n()"])],
            3,
        );
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
