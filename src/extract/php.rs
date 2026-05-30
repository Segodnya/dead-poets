//! PHP adapter — classify call sites, decode key arguments, recognise literals.
//!
//! Calls: `function_call_expression` (bare name), `member_call_expression` and
//! `scoped_call_expression` (method, matched later on a normalized receiver:
//! `$i18n` → `i18n`, `$this->i18n` → `this.i18n`, `Container::get_i18n()` →
//! `get_i18n()`). PHP has no index form. Strings decode per single/double-quote
//! rules; `'a' . $x` flattens to segments.

use tree_sitter::Node;

use crate::decode::{Decoded, Lang, decode_token, unescape_php_double};
use crate::guard::Segment;

use super::{AstMatcher, CallShape, binary_operator, concat_segments, text};

/// Zero-sized adapter selected for `SourceLang::Php`.
pub(super) struct Php;

impl AstMatcher for Php {
    fn classify<'a>(node: Node<'a>, src: &str) -> Option<CallShape<'a>> {
        match node.kind() {
            "function_call_expression" => {
                let func = node.child_by_field_name("function")?;
                if func.kind() != "name" {
                    return None;
                }
                let name = text(func, src).to_string();
                let args = arg_values(node.child_by_field_name("arguments")?);
                Some(CallShape::Function { name, args })
            }
            "member_call_expression" => {
                let name_node = node.child_by_field_name("name")?;
                let obj = node.child_by_field_name("object")?;
                let name = text(name_node, src).to_string();
                let receiver = normalize_receiver(obj, src);
                let args = arg_values(node.child_by_field_name("arguments")?);
                Some(CallShape::Method {
                    name,
                    receiver,
                    args,
                })
            }
            "scoped_call_expression" => {
                let name_node = node.child_by_field_name("name")?;
                let scope = node.child_by_field_name("scope")?;
                let name = text(name_node, src).to_string();
                let receiver = text(scope, src).trim_start_matches('$').to_string();
                let args = arg_values(node.child_by_field_name("arguments")?);
                Some(CallShape::Method {
                    name,
                    receiver,
                    args,
                })
            }
            _ => None,
        }
    }

    fn segments(node: Node, src: &str) -> Vec<Segment> {
        segments(node, src)
    }

    fn literal(node: Node, src: &str) -> Option<Vec<Segment>> {
        match node.kind() {
            "string" | "encapsed_string" => Some(segments(node, src)),
            _ => None,
        }
    }
}

/// Collect PHP argument *value* nodes (unwrapping each `argument`).
fn arg_values(args: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut cursor = args.walk();
    for child in args.named_children(&mut cursor) {
        if child.kind() == "argument" {
            let count = child.named_child_count();
            if count > 0
                && let Some(value) = child.named_child(count as u32 - 1)
            {
                out.push(value);
            }
        }
    }
    out
}

fn normalize_receiver(node: Node, src: &str) -> String {
    match node.kind() {
        "variable_name" => text(node, src).trim_start_matches('$').to_string(),
        "name" => text(node, src).to_string(),
        "member_access_expression" => {
            match (
                node.child_by_field_name("object"),
                node.child_by_field_name("name"),
            ) {
                (Some(obj), Some(name)) => {
                    format!("{}.{}", normalize_receiver(obj, src), text(name, src))
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

fn segments(node: Node, src: &str) -> Vec<Segment> {
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
            concat_segments(node, src, segments)
        }
        _ => vec![Segment::Hole],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .unwrap();
        p.parse(src, None).unwrap()
    }

    fn first_shape<'a>(node: Node<'a>, src: &str) -> Option<CallShape<'a>> {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if let Some(shape) = Php::classify(n, src) {
                return Some(shape);
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        None
    }

    /// The adapter is testable in isolation: classify a method call straight to a
    /// shape with its name and normalized receiver — no full `extract()` needed.
    #[test]
    fn classify_method_normalizes_receiver() {
        let src = "<?php $this->i18n->get('k'); ?>";
        let tree = parse(src);
        match first_shape(tree.root_node(), src) {
            Some(CallShape::Method { name, receiver, .. }) => {
                assert_eq!(name, "get");
                assert_eq!(receiver, "this.i18n");
            }
            other => panic!("expected Method shape, got {:?}", other.is_some()),
        }
    }

    /// Single-quote escaping is the adapter's concern; `\n` stays literal.
    #[test]
    fn segments_single_quote_keeps_backslash_n() {
        let src = "<?php $x = 'a\\nb'; ?>";
        let tree = parse(src);
        // Walk to the `string` node.
        let mut stack = vec![tree.root_node()];
        let mut found = None;
        while let Some(n) = stack.pop() {
            if n.kind() == "string" {
                found = Some(segments(n, src));
                break;
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        assert_eq!(found, Some(vec![Segment::Static("a\\nb".to_string())]));
    }
}
