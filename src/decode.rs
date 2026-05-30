//! Literal decoding — the single "plain literal vs guard" decision point.
//!
//! Turns a source string token (quotes included) into its **canonical runtime
//! string**, or reports it as *dynamic* (interpolated) so the caller routes it
//! to the guard path. All comparison downstream is canonical-to-canonical, so a
//! PHP `"a\nb"` and a PO `msgid "a\nb"` (both a real newline) compare equal.
//!
//! Per-language escape rules differ and matter:
//! - PHP single-quote `'…'`: only `\\` and `\'` are escapes; `\n` is literal.
//! - PHP double-quote `"…"`: full escapes; any unescaped `$` → interpolation.
//! - JS `'…'` / `"…"`: full escapes, no interpolation.
//! - JS template `` `…` ``: full escapes; an unescaped `${` → interpolation.
//! - Twig `'…'` / `"…"`: `\\` / quote escapes; Twig `"#{…}"` → interpolation.

/// The source language of a token, selecting the escape/interpolation rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Php,
    Js,
    Twig,
}

/// The outcome of decoding a string token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// A plain, fully-static literal with its canonical runtime value.
    /// Goes into the literal set.
    Literal(String),
    /// The token contains interpolation; it is not a plain literal and must be
    /// routed to the guard layer for static-fragment extraction.
    Dynamic,
}

/// Decode a string token (with surrounding quotes) for the given language.
///
/// The quote character selects the sub-rule. A token shorter than two chars
/// (no closing quote) is treated as an empty literal.
pub fn decode_token(lang: Lang, token: &str) -> Decoded {
    let Some((quote, inner)) = split_quotes(token) else {
        return Decoded::Literal(String::new());
    };
    match (lang, quote) {
        (Lang::Php, '"') => {
            if has_php_interpolation(inner) {
                Decoded::Dynamic
            } else {
                Decoded::Literal(unescape_php_double(inner))
            }
        }
        // PHP single-quote (and any other quote, e.g. backtick) use single rules.
        (Lang::Php, _) => Decoded::Literal(unescape_php_single(inner)),
        (Lang::Js, '`') => {
            if has_template_interpolation(inner) {
                Decoded::Dynamic
            } else {
                Decoded::Literal(unescape_js(inner))
            }
        }
        (Lang::Js, _) => Decoded::Literal(unescape_js(inner)),
        (Lang::Twig, '"') => {
            if inner.contains("#{") {
                Decoded::Dynamic
            } else {
                Decoded::Literal(unescape_twig(inner))
            }
        }
        (Lang::Twig, _) => Decoded::Literal(unescape_twig(inner)),
    }
}

/// Split a quoted token into its quote char and inner content. Returns `None`
/// when the token has no surrounding quote pair.
fn split_quotes(token: &str) -> Option<(char, &str)> {
    let mut chars = token.chars();
    let first = chars.next()?;
    if !matches!(first, '\'' | '"' | '`') {
        return None;
    }
    let inner = chars.as_str();
    // Strip the trailing quote if it matches.
    let inner = inner.strip_suffix(first).unwrap_or(inner);
    Some((first, inner))
}

/// True if the PHP double-quoted body contains an unescaped `$` (variable or
/// `{$expr}` interpolation).
fn has_php_interpolation(inner: &str) -> bool {
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '$' => return true,
            _ => {}
        }
    }
    false
}

/// True if the JS template body contains an unescaped `${`.
fn has_template_interpolation(inner: &str) -> bool {
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '$' if chars.peek() == Some(&'{') => return true,
            _ => {}
        }
    }
    false
}

/// PHP single-quote: only `\\` and `\'` are escapes; every other backslash is
/// literal.
pub fn unescape_php_single(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                Some('\'') => {
                    out.push('\'');
                    chars.next();
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// PHP double-quote escapes. Unrecognized escapes keep the backslash (PHP
/// semantics).
pub fn unescape_php_double(inner: &str) -> String {
    decode_escapes(inner, EscapeFlavor::PhpDouble)
}

/// JS string/template escapes. Unrecognized escapes drop the backslash (JS
/// semantics).
pub fn unescape_js(inner: &str) -> String {
    decode_escapes(inner, EscapeFlavor::Js)
}

/// Twig string escapes: `\\` and the quote characters.
pub fn unescape_twig(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                Some('\'') => {
                    out.push('\'');
                    chars.next();
                }
                Some('"') => {
                    out.push('"');
                    chars.next();
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Clone, Copy)]
enum EscapeFlavor {
    PhpDouble,
    Js,
}

/// Shared C-like escape decoder for PHP double-quote and JS contexts.
fn decode_escapes(inner: &str, flavor: EscapeFlavor) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('`') => out.push('`'),
            Some('$') => out.push('$'),
            Some('0') => out.push('\0'),
            Some('v') => out.push('\u{0b}'),
            Some('f') => out.push('\u{0c}'),
            Some('b') => out.push('\u{08}'),
            Some('e') if matches!(flavor, EscapeFlavor::PhpDouble) => out.push('\u{1b}'),
            Some(other) => match flavor {
                // PHP keeps the backslash for unrecognized escapes.
                EscapeFlavor::PhpDouble => {
                    out.push('\\');
                    out.push(other);
                }
                // JS drops the backslash.
                EscapeFlavor::Js => out.push(other),
            },
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_single_quote_keeps_backslash_n_literal() {
        assert_eq!(
            decode_token(Lang::Php, "'a\\nb'"),
            Decoded::Literal("a\\nb".to_string())
        );
        assert_eq!(
            decode_token(Lang::Php, "'it\\'s'"),
            Decoded::Literal("it's".to_string())
        );
    }

    #[test]
    fn php_double_quote_decodes_escapes() {
        // "a\nb" -> a, real newline, b
        assert_eq!(
            decode_token(Lang::Php, "\"a\\nb\""),
            Decoded::Literal("a\nb".to_string())
        );
    }

    #[test]
    fn php_double_quote_with_variable_is_dynamic() {
        assert_eq!(decode_token(Lang::Php, "\"role_$x\""), Decoded::Dynamic);
        assert_eq!(
            decode_token(Lang::Php, "\"user_{$role}\""),
            Decoded::Dynamic
        );
        // escaped dollar is NOT interpolation
        assert_eq!(
            decode_token(Lang::Php, "\"price_\\$5\""),
            Decoded::Literal("price_$5".to_string())
        );
    }

    #[test]
    fn js_plain_and_template_literals() {
        assert_eq!(
            decode_token(Lang::Js, "'plain'"),
            Decoded::Literal("plain".to_string())
        );
        assert_eq!(
            decode_token(Lang::Js, "`plain`"),
            Decoded::Literal("plain".to_string())
        );
        assert_eq!(
            decode_token(Lang::Js, "\"a\\tb\""),
            Decoded::Literal("a\tb".to_string())
        );
    }

    #[test]
    fn js_template_with_interpolation_is_dynamic() {
        assert_eq!(
            decode_token(Lang::Js, "`cf_subtype_${x}`"),
            Decoded::Dynamic
        );
        // escaped ${ is literal
        assert_eq!(
            decode_token(Lang::Js, "`cost_\\${x}`"),
            Decoded::Literal("cost_${x}".to_string())
        );
    }

    #[test]
    fn twig_string_literal() {
        assert_eq!(
            decode_token(Lang::Twig, "'key'"),
            Decoded::Literal("key".to_string())
        );
        assert_eq!(
            decode_token(Lang::Twig, "\"key\""),
            Decoded::Literal("key".to_string())
        );
        assert_eq!(decode_token(Lang::Twig, "\"x#{y}\""), Decoded::Dynamic);
    }

    /// Canonical-to-canonical: PHP "a\nb" decodes to the same string a PO file's
    /// already-decoded `a\nb` msgid would be.
    #[test]
    fn canonical_php_matches_po_decoded() {
        let php = decode_token(Lang::Php, "\"a\\nb\"");
        let po_side = "a\nb".to_string(); // what polib hands back
        assert_eq!(php, Decoded::Literal(po_side));
    }
}
