//! JS/TS adapter — shared by the JS, JSX, TS, and TSX grammars (the grammar is
//! selected upstream in `ParserPool`; the matching behaviour is identical).
//!
//! Calls: `call_expression` on a bare `identifier` (function) or a
//! `member_expression` (method, matched later on a normalized receiver).
//! Index: `subscript_expression` — `locale['key']` — with the object normalized
//! the same way. Strings decode per quote/template rules; `'a' + x` flattens to
//! segments.

use tree_sitter::Node;

use crate::decode::{Decoded, Lang, decode_token, unescape_js};
use crate::guard::Segment;

use super::{AstMatcher, CallShape, binary_operator, concat_segments, text};

/// Zero-sized adapter selected for the JS family (`Js`/`Jsx`/`Ts`/`Tsx`).
pub(super) struct Js;

impl AstMatcher for Js {
    fn classify<'a>(node: Node<'a>, src: &str) -> Option<CallShape<'a>> {
        match node.kind() {
            "call_expression" => {
                let func = node.child_by_field_name("function")?;
                match func.kind() {
                    "identifier" => {
                        let name = text(func, src).to_string();
                        let args = arg_values(node.child_by_field_name("arguments")?);
                        Some(CallShape::Function { name, args })
                    }
                    "member_expression" => {
                        let prop = func.child_by_field_name("property")?;
                        let obj = func.child_by_field_name("object")?;
                        let name = text(prop, src).to_string();
                        let receiver = normalize_receiver(obj, src);
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
            "subscript_expression" => {
                let obj = node.child_by_field_name("object")?;
                let key = node.child_by_field_name("index")?;
                let object = normalize_receiver(obj, src);
                Some(CallShape::Index { object, key })
            }
            _ => None,
        }
    }

    fn segments(node: Node, src: &str) -> Vec<Segment> {
        segments(node, src)
    }

    fn literal(node: Node, src: &str) -> Option<Vec<Segment>> {
        match node.kind() {
            "string" | "template_string" => Some(segments(node, src)),
            _ => None,
        }
    }
}

/// Collect JS call argument nodes, skipping comments.
fn arg_values(args: Node) -> Vec<Node> {
    let mut values = Vec::new();
    let mut cursor = args.walk();
    for child in args.named_children(&mut cursor) {
        if child.kind() != "comment" {
            values.push(child);
        }
    }
    values
}

fn normalize_receiver(node: Node, src: &str) -> String {
    match node.kind() {
        "identifier" | "property_identifier" => text(node, src).to_string(),
        "this" => "this".to_string(),
        "member_expression" => {
            match (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            ) {
                (Some(obj), Some(prop)) => {
                    format!("{}.{}", normalize_receiver(obj, src), text(prop, src))
                }
                _ => text(node, src).to_string(),
            }
        }
        _ => text(node, src).to_string(),
    }
}

fn segments(node: Node, src: &str) -> Vec<Segment> {
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
                    "escape_sequence" => segs.push(Segment::Static(unescape_js(text(child, src)))),
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
        p.set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        p.parse(src, None).unwrap()
    }

    fn first_shape<'a>(node: Node<'a>, src: &str) -> Option<CallShape<'a>> {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if let Some(shape) = Js::classify(n, src) {
                return Some(shape);
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        None
    }

    /// An index access classifies straight to its normalized object — testable
    /// without driving a full `extract()`.
    #[test]
    fn classify_index_normalizes_object() {
        let src = "this.locale['Delete email'];";
        let tree = parse(src);
        match first_shape(tree.root_node(), src) {
            Some(CallShape::Index { object, .. }) => assert_eq!(object, "this.locale"),
            other => panic!("expected Index shape, got {:?}", other.is_some()),
        }
    }

    /// Template interpolation becomes a hole; the static prefix stays a fragment.
    #[test]
    fn segments_template_prefix_and_hole() {
        let src = "const k = `cf_${x}`;";
        let tree = parse(src);
        let mut stack = vec![tree.root_node()];
        let mut found = None;
        while let Some(n) = stack.pop() {
            if n.kind() == "template_string" {
                found = Some(segments(n, src));
                break;
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                stack.push(ch);
            }
        }
        assert_eq!(
            found,
            Some(vec![Segment::Static("cf_".to_string()), Segment::Hole])
        );
    }
}
