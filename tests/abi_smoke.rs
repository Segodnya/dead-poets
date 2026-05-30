//! Phase 0 — ABI smoke test.
//! Confirms every pinned tree-sitter grammar loads into a Parser and parses
//! a trivial snippet without panic / ABI mismatch (PLAN Addendum 2 §6).

use tree_sitter::Parser;

fn parses(lang: tree_sitter::Language, src: &str) -> bool {
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language failed (ABI mismatch?)");
    let tree = parser.parse(src, None).expect("parse returned None");
    !tree.root_node().has_error()
}

#[test]
fn php_grammar_loads() {
    // HTML-aware grammar (LANGUAGE_PHP, not LANGUAGE_PHP_ONLY) per §6.
    assert!(parses(tree_sitter_php::LANGUAGE_PHP.into(), "<?php __('hello'); ?>"));
}

#[test]
fn javascript_grammar_loads() {
    assert!(parses(tree_sitter_javascript::LANGUAGE.into(), "i18n('hello');"));
}

#[test]
fn typescript_grammar_loads() {
    assert!(parses(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), "const x: string = i18n('h');"));
}

#[test]
fn tsx_grammar_loads() {
    assert!(parses(tree_sitter_typescript::LANGUAGE_TSX.into(), "const e = <div>{i18n('h')}</div>;"));
}
