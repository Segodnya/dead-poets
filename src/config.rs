//! Configuration model (`dead-poets.toml`).
//!
//! Everything project-specific lives here, never in code (PLAN: "general crate,
//! repo as testbed"). The schema must be expressive enough to describe a project
//! like `example_repo` without code changes — hence the generalized `[[calls]]`
//! model (function / method+receiver / filter).

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Top-level config. Missing tables fall back to their defaults.
#[derive(Debug, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub scan: Scan,
    /// Translation call sites to look for. `[[calls]]` array.
    #[serde(default)]
    pub calls: Vec<CallSpec>,
    #[serde(default)]
    pub output: Output,
    #[serde(default)]
    pub whitelist: Whitelist,
}

/// `[scan]` — where to find PO files and source.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Scan {
    /// Glob patterns for PO/POT catalogs.
    pub po_patterns: Vec<String>,
    /// Source file extensions to scan.
    pub source_extensions: Vec<String>,
    /// Extra directories to ignore (on top of `.gitignore`).
    pub ignore_dirs: Vec<String>,
    /// Source roots. Always a list, even for a single root, so multi-repo
    /// coverage is a config change, not a code change (PLAN §7).
    pub source_roots: Vec<String>,
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            po_patterns: vec!["**/*.po".into(), "**/*.pot".into()],
            source_extensions: ["php", "twig", "js", "ts", "jsx", "tsx"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ignore_dirs: ["vendor", "node_modules", "cache", "var"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            source_roots: vec![".".into()],
        }
    }
}

/// What kind of call site this `[[calls]]` entry describes.
#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum CallKind {
    /// Free function: `i18n('key')`.
    Function,
    /// Method with a receiver constraint: `$i18n->get('key')`.
    Method,
    /// Twig filter: `{{ 'key'|i18n }}`.
    Filter,
    /// Index access into a translation map: `locale['key']` (JS lang bundles).
    /// `name` holds the indexed object (`locale`); the subscript is the key.
    Index,
}

/// A single translation call site descriptor (`[[calls]]`).
#[derive(Debug, Deserialize, Clone)]
pub struct CallSpec {
    /// Language this matcher applies to (`php`, `js`, `twig`, ...).
    pub lang: String,
    pub kind: CallKind,
    /// Function / method / filter name — or, for `index`, the indexed object
    /// identifier (`locale` in `locale['key']`).
    pub name: String,
    /// For `method`: allowed receivers (e.g. `["i18n", "this.i18n"]`). The `$`
    /// and `->`/`.` are normalized away by the extractor.
    #[serde(default)]
    pub receiver: Option<Vec<String>>,
    /// Which argument holds the key (0-based). Defaults to 0.
    #[serde(default)]
    pub key_arg_index: usize,
}

/// `[output]` — reporting and exit-code policy.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Output {
    pub mode: OutputMode,
    pub format: OutputFormat,
    pub fail_on: FailOn,
    /// Minimum static-fragment length that may form a guard (PLAN §1).
    pub min_guard_len: usize,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            mode: OutputMode::default(),
            format: OutputFormat::default(),
            fail_on: FailOn::default(),
            min_guard_len: 3,
        }
    }
}

/// Output mode. Only `review` exists in v1; deletion / export are future work.
#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    #[default]
    Review,
}

/// Report format.
#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

/// Exit-code policy (PLAN §8).
#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum FailOn {
    Never,
    #[default]
    Dead,
    DeadOrBlind,
}

/// `[whitelist]` — keys to always treat as alive (dynamic keys static analysis
/// can't see: DB/config/external).
#[derive(Debug, Deserialize, Default)]
pub struct Whitelist {
    /// Path to a file with one key per line (relative to the config dir).
    #[serde(default)]
    pub file: Option<String>,
    /// Inline keys.
    #[serde(default)]
    pub keys: Vec<String>,
}

impl Config {
    /// Load and parse a config file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file: {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("invalid config: {}", path.display()))?;
        Ok(cfg)
    }
}

/// Merge inline whitelist keys with keys read from the whitelist file (if any),
/// resolving the file relative to `base_dir`. A missing file is a hard, clearly
/// labelled error so a typo'd path never silently disables the whitelist.
pub fn resolve_whitelist(wl: &Whitelist, base_dir: &Path) -> Result<HashSet<String>> {
    let mut set: HashSet<String> = wl.keys.iter().cloned().collect();
    if let Some(file) = &wl.file {
        let path = base_dir.join(file);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("whitelist file not found: {}", path.display()))?;
        for line in content.lines() {
            let key = line.trim();
            if !key.is_empty() && !key.starts_with('#') {
                set.insert(key.to_string());
            }
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example_repo config from PLAN must deserialize into three correct
    /// call specs, and the omitted `source_roots` must default to `["."]`.
    #[test]
    fn parses_example_repo_config() {
        let toml_src = r#"
            [scan]
            po_patterns = ["app/libs/locale/locales/*/LC_MESSAGES/messages.po"]
            source_extensions = ["php", "twig", "js", "ts", "jsx", "tsx"]
            ignore_dirs = ["vendor", "node_modules", "cache", "build"]

            [[calls]]
            lang = "php"
            kind = "method"
            name = "get"
            receiver = ["i18n", "this.i18n"]
            key_arg_index = 0

            [[calls]]
            lang = "js"
            kind = "function"
            name = "i18n"
            key_arg_index = 0

            [[calls]]
            lang = "twig"
            kind = "filter"
            name = "i18n"

            [output]
            mode = "review"
            format = "text"
            fail_on = "dead"
        "#;
        let cfg: Config = toml::from_str(toml_src).expect("should deserialize");

        // source_roots omitted -> default ["."]
        assert_eq!(cfg.scan.source_roots, vec![".".to_string()]);

        assert_eq!(cfg.calls.len(), 3);

        let php = &cfg.calls[0];
        assert_eq!(php.lang, "php");
        assert_eq!(php.kind, CallKind::Method);
        assert_eq!(php.name, "get");
        assert_eq!(
            php.receiver.as_deref(),
            Some(["i18n".to_string(), "this.i18n".to_string()].as_slice())
        );
        assert_eq!(php.key_arg_index, 0);

        assert_eq!(cfg.calls[1].kind, CallKind::Function);
        assert_eq!(cfg.calls[1].name, "i18n");

        let twig = &cfg.calls[2];
        assert_eq!(twig.kind, CallKind::Filter);
        assert!(twig.receiver.is_none());
    }

    /// Omitted fields fall back to documented defaults.
    #[test]
    fn defaults_apply_when_fields_missing() {
        let cfg: Config = toml::from_str("").expect("empty config is valid");
        assert_eq!(cfg.output.min_guard_len, 3);
        assert_eq!(cfg.output.fail_on, FailOn::Dead);
        assert_eq!(cfg.output.format, OutputFormat::Text);
        assert_eq!(cfg.output.mode, OutputMode::Review);
        assert_eq!(cfg.scan.source_roots, vec![".".to_string()]);
        // a [[calls]] entry without key_arg_index defaults to 0
        let cfg2: Config =
            toml::from_str("[[calls]]\nlang=\"js\"\nkind=\"function\"\nname=\"i18n\"").unwrap();
        assert_eq!(cfg2.calls[0].key_arg_index, 0);
    }

    /// `dead-or-blind` is a valid fail_on value (kebab-case mapping).
    #[test]
    fn fail_on_kebab_case_roundtrips() {
        let cfg: Config =
            toml::from_str("[output]\nfail_on = \"dead-or-blind\"").unwrap();
        assert_eq!(cfg.output.fail_on, FailOn::DeadOrBlind);
    }

    /// An unknown fail_on value is a configuration error.
    #[test]
    fn unknown_fail_on_is_error() {
        let err = toml::from_str::<Config>("[output]\nfail_on = \"sometimes\"");
        assert!(err.is_err(), "unknown fail_on must fail to parse");
    }

    /// File keys and inline keys merge into one set.
    #[test]
    fn whitelist_merges_file_and_inline() {
        let dir = std::env::temp_dir().join("dead-poets-wl-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("wl.txt");
        std::fs::write(&file, "from.file.1\n# comment\n\nfrom.file.2\n").unwrap();

        let wl = Whitelist {
            file: Some("wl.txt".to_string()),
            keys: vec!["inline.1".to_string()],
        };
        let set = resolve_whitelist(&wl, &dir).unwrap();
        assert_eq!(set.len(), 3);
        assert!(set.contains("from.file.1"));
        assert!(set.contains("from.file.2"));
        assert!(set.contains("inline.1"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A missing whitelist file produces a clear error mentioning the path.
    #[test]
    fn whitelist_missing_file_is_clear_error() {
        let wl = Whitelist {
            file: Some("nope.txt".to_string()),
            keys: vec![],
        };
        let err = resolve_whitelist(&wl, Path::new("/tmp/definitely-missing-dir-xyz"))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("nope.txt"), "error should name the path: {msg}");
    }
}
