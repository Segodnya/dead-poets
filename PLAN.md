# PLAN

Here’s a combined and translated version of both responses — a complete guide to building **dead-poets**, a Rust tool to find unused gettext PO keys in projects mixing PHP, Twig, JS, and TS.

---

## What already exists (and why you might still need your own tool)

Out‑of‑the‑box solutions for detecting unused gettext keys across a polyglot codebase are extremely rare. Most options are language‑specific or work as IDE plugins, not as a standalone static analyzer that can traverse the whole project.

- **Poedit** can flag “unused strings” if you configure a Source code parser (Catalogue → Properties → Source keywords). You could write an extractor in Python/PHP that returns the set of keys actually used in your code. But it’s cumbersome, slow on large projects, and you have to maintain the extractor.
- **PhpStorm / IntelliJ** has an “Unused property key” inspection, but PO support is weak — it’s designed for JSON/YAML resources. For pure gettext you’d need a custom plugin or rely on regex searches.
- Dedicated JS linters like `eslint-plugin-i18n` or `i18n-unused` won’t cover PHP and Twig. Chaining them together with scripts is essentially building your own solution anyway.

So a custom Rust analyzer makes perfect sense: it will be fast, cross‑platform, and able to understand the AST of all your target languages at once.

---

## Challenges beyond simple concatenation

1. **Dynamic keys** — not just concatenation of literals, but also:
   ```php
   __("user_role_{$role}");  // interpolation
   __($someVariable);        // key entirely from a variable
   ```
   A static analyzer can’t resolve these, leaving many false positives.

2. **Plural forms (`ngettext` / `dngettext`)** — the PO key is the pair (`msgid`, `msgid_plural`). Usage looks like `ngettext('1 item', '%d items', $count)`. The tool must check both forms.

3. **Context (`pgettext`)** — the key is `(msgctxt, msgid)`. Code uses `pgettext('menu', 'Open')`. Failing to match context yields false “unused” reports.

4. **Translation domains (`dgettext` / `dcgettext`)** — a key may reside in one PO file but be called with a different domain. Domain–key mapping must be tracked.

5. **Twig peculiarities** — translations appear via the `|trans` filter, `{% trans %}` tags, or custom functions like `t()`. Sometimes the string is written directly inside a block (`{% trans %}Hello{% endtrans %}`), and the PO key is exactly `Hello`. The tool must handle both explicit key calls and block‑level text.

6. **Format specifiers (`sprintf`)** — keys often contain `%s`, `%d` but in code they’re split across arguments, e.g., `__('Welcome %s', $name)`. The key is `'Welcome %s'`; the analyzer should extract the first argument and ignore the rest.

7. **Keys from databases / configs / external APIs** — static analysis can’t see them. Plan to whitelist these keys or exclude them from deletion.

8. **Obsolete and fuzzy entries** — they aren’t active translations. Either ignore them during checking or flag them separately.

9. **Template inheritance and includes** — a key might be used in a parent Twig template, not the one being scanned. For simplicity, scanning all files (including parent templates) usually suffices.

10. **Shortened call forms** — JS/TS often use wrappers like `i18n.t('key')` or `$t('key')`. A configurable list of function names is essential.

---

## How a Rust tool could work (high-level design)

- Accept the project root and an optional config file.
- Collect all `.po` / `.pot` files, parse them (the PO format is simple; use a crate or write a parser). Extract unique keys, respecting context and plural forms.
- Find source files by extension: `*.php`, `*.twig`, `*.js`, `*.ts`, `*.jsx`, `*.tsx`.
- For each language, attach the appropriate `tree-sitter` parser, traverse the AST, and locate translation function calls. Extract literal key strings.
- For simple concatenations (both operands are string literals), evaluate the result at traversal time.
- Compare the set of keys from PO files with the set found in code, and list unused ones.
- Classify each key with a single status (see Addendum 2 §3): `Dead` / `Alive`, plus `alive_via: literal | guard` on live keys. (The earlier low/medium/high confidence scale is superseded.)

---

## Crates you’ll need

| Purpose | Crate | Notes |
|---------|-------|-------|
| PO file parsing | [`poparser`](https://crates.io/crates/poparser) | Mature, handles `msgid`, `msgid_plural`, `msgctxt`, headers, fuzzy/obsolete flags. |
| PHP AST | [`tree-sitter`](https://crates.io/crates/tree-sitter) + [`tree-sitter-php`](https://crates.io/crates/tree-sitter-php) | Parses PHP 7/8. |
| JavaScript / TypeScript AST | [`tree-sitter-javascript`](https://crates.io/crates/tree-sitter-javascript), [`tree-sitter-typescript`](https://crates.io/crates/tree-sitter-typescript) | For `.js`, `.ts`, `.jsx`, `.tsx`. The TypeScript grammar covers TSX too. |
| Twig handling | No stable tree‑sitter grammar available. **Recommended:** a smart regex scanner for `|trans`, `t()`, and `{% trans %}` blocks. For compiled Twig templates you could use PHP‑parser on the cached PHP files, but it’s more complex. For v1, regex is fine. | Use Rust’s built‑in `regex` crate. |
| File system traversal | [`ignore`](https://crates.io/crates/ignore) | Respects `.gitignore`, faster than `walkdir`, filters early. |
| CLI interface | [`clap`](https://crates.io/crates/clap) with `derive` feature | Simple, powerful argument parsing. |
| Configuration | [`serde`](https://crates.io/crates/serde) + [`toml`](https://crates.io/crates/toml) | `dead-poets.toml` at the project root. |
| Error handling | [`anyhow`](https://crates.io/crates/anyhow) | Great for prototypes. |
| Logging | [`log`](https://crates.io/crates/log) + [`env_logger`](https://crates.io/crates/env_logger) | For verbose progress output. |
| Colored output | [`colored`](https://crates.io/crates/colored) | Highlights unused keys. |
| Machine‑readable output (optional) | [`serde_json`](https://crates.io/crates/serde_json) | Export to JSON for CI. |

---

## Integrating tree‑sitter

### 1. Initializing parsers

```rust
use tree_sitter::Parser;

let php_lang = tree_sitter_php::language();
let mut php_parser = Parser::new();
php_parser.set_language(php_lang)?;

let js_lang = tree_sitter_javascript::language();
let mut js_parser = Parser::new();
js_parser.set_language(js_lang)?;

let ts_lang = tree_sitter_typescript::language_typescript();
let mut ts_parser = Parser::new();
ts_parser.set_language(ts_lang)?;
// For TSX use language_tsx()
```

### 2. Finding translation calls with tree‑sitter queries

For PHP functions like `__`, `gettext`, `ngettext`:

```scheme
; Find function calls with a name from the config
(function_call_expression
  function: (name) @func
  arguments: (arguments (argument (string) @str)?)
)
```

Rust code:

```rust
let query = tree_sitter::Query::new(
    php_lang,
    r#"
    (function_call_expression
      function: (name) @func
      arguments: (arguments (argument (string) @str)?)
    )
    "#
)?;

let mut cursor = tree_sitter::QueryCursor::new();
for m in cursor.matches(&query, tree.root_node(), source.as_bytes()) {
    let func_name = m.captures.iter()
        .find(|c| c.name == "func")
        .map(|c| c.node.utf8_text(source.as_bytes()).unwrap());
    // If func_name is in the configured list, collect @str
}
```

For `ngettext`, `pgettext`, etc., the number of arguments differs. You can write separate queries or check the argument count and pick the right string node.

### 3. Resolving string concatenation

When a key is built by concatenating two string literals:

```php
__('Hello, ' . $username); // key is "Hello, "
```

If both sides are literal strings, the analyzer can evaluate the result. Traverse the AST; if the argument is a `binary_expression` with operator `.` and both children are `string` nodes, concatenate their values. For anything more complex, skip and mark the key as unresolved.

---

## Twig strategy (regex‑based fallback)

Regular expressions to catch common Twig patterns:

1. `{{ 'key'|trans }}`  
2. `{% trans %}Key text{% endtrans %}`  
3. `{{ t('key', ...) }}` (helper function)  
4. `{{ "key"|trans }}`

Patterns:

```rust
let re_filter = Regex::new(r#"['"]([^'"]+)['"]\s*\|\s*trans"#)?;
let re_block = Regex::new(r#"{%\s*trans\s*%}(.*?){%\s*endtrans\s*%}"#)?;
let re_func = Regex::new(r#"t\s*\(\s*['"]([^'"]+)['"]"#)?;
```

For block translations with variables (`{% trans %}Hello {{ name }}{% endtrans %}`), Twig normally converts them to keys with `%name%` placeholders. You may need to normalize such strings to match PO entries. For v1, just collect the raw text and count it as a potential key.

---

## CLI and configuration

### CLI (clap derive)

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dead-poets")]
#[command(about = "Find unused gettext keys in your project")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan the project for unused PO keys
    Scan {
        /// Root directory of the project
        #[arg(default_value = ".")]
        path: String,

        /// Path to config file
        #[arg(short, long, default_value = "dead-poets.toml")]
        config: String,

        /// Output format: text, json, csv
        #[arg(short, long, default_value = "text")]
        format: String,

        /// Verbosity level
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,
    },
}
```

### Config file `dead-poets.toml`

```toml
[scan]
# Where to look for PO files (glob)
po_patterns = ["**/*.po", "**/*.pot"]
# Source file extensions
source_extensions = ["php", "twig", "js", "ts", "jsx", "tsx"]
# Directories to ignore (in addition to .gitignore)
ignore_dirs = ["vendor", "node_modules", "cache", "var"]

[functions]
# Translation function names per language
php = ["__", "gettext", "ngettext", "pgettext", "dgettext", "dcgettext"]
js = ["i18n.t", "$t", "t"]
twig = ["trans", "t"]   # t() is a custom helper

[whitelist]
# Path to a file with one key per line
file = "i18n-whitelist.txt"
# Or inline array
keys = [
    "dynamic.key1",
    "dynamic.key2"
]

[output]
# See Addendum 2 §3/§8 for the resolved output + exit-code model.
format = "text"      # text | json
fail_on = "dead"     # never | dead | dead-or-blind
```

---

## Handling plural forms, context, and domains

In a PO file, the key is the tuple `(msgctxt, msgid, msgid_plural?)`. `poparser` exposes these fields. A call to `ngettext('1 apple', '%d apples', count)` yields two literals that together form the key. The analyzer extracts both and looks them up in the PO index.

Build two indices for comparison:
- `HashSet<String>` — for simple `msgid`.
- `HashMap<(Option<String>, String, Option<String>), bool>` — for contextual and plural keys.

For domains, if your config lists functions like `dgettext("domain", "key")`, store a mapping of domain to PO file. The tool can then verify that the key exists in the correct domain’s catalog.

---

## Potential pitfalls and mitigations

| Challenge | Mitigation |
|-----------|------------|
| Key built from variables | Emit a guard from static fragments (Addendum 2 §1); if none qualify, count as a blind site. |
| Dynamic keys with interpolation (`"prefix_$suffix"` in PHP) | tree‑sitter gives an `encapsed` node with parts; extract static fragments → guards; a fragment shorter than `min_guard_len` does **not** create a guard (it would over-keep), it increments `blind_count`. |
| Twig with variables inside `{% trans %}` | Block translations become keys with placeholders; normalization requires knowledge of the Twig extension’s rules. Defer to v2. |
| Multiple domains | Map domains to PO files in config; track domain–key pairs. |
| Large projects | Parsing with `tree-sitter` in Rust is fast, but use `rayon` for parallel file processing and `DashMap` for thread‑safe key collection. |
| False positives | Bias toward keep via guards; surface `alive_via` and the blind-spot summary; whitelist essential keys. |

---

## v1 feature checklist for dead‑poets

- Parse PO files with `poparser`.
- Walk `.php`, `.js`, `.ts` using tree-sitter grammars; use regex for `.twig`.
- Resolve concatenation of two string literals.
- Support `__`, `gettext`, `ngettext`, `pgettext` and their JS counterparts (configurable).
- CLI and configuration as described above.
- Colored output of unused keys, with optional JSON export for CI.

This will cover 90% of real‑world needs and, thanks to Rust, will run extremely fast even on monorepos.

---

# Addendum — refined direction after exploring the first real target

> The sections above are the original design and still stand. This addendum records the
> conclusions reached after digging into the problem and exploring `example_repo`, the **first
> real-world repo we'll test the crate against**. Nothing here narrows the tool to one repo — the
> crate stays a **general, standalone, configurable CLI**. `example_repo` is the proving ground;
> its specifics become an example config and the first integration test, not hardcoded assumptions.

## Product stance: general crate, repo as testbed

- **dead-poets is a reusable library + standalone CLI.** Everything repo-specific (wrapper names,
  receivers, file globs, output mode) lives in `dead-poets.toml`, never in the code.
- `example_repo` is where we *validate* the tool. We ship an example config for it
  (`examples/example_repo.toml`) and use it as the first end-to-end fixture, but the engine knows
  nothing about it.
- This means the config schema must be expressive enough to describe a project like `example_repo`
  without code changes — see "Config implications" below.

## The single most important insight: dynamics dominate, and tree-sitter can't expand them

The original plan treats dynamic keys as a secondary "confidence" concern. In a real codebase they
are **the central risk**, and they are precisely what an AST parser *cannot* solve (`${var}` is not
statically resolvable). Observed in `example_repo`:

- **JS/TS: ~40–50%** of translation calls are template strings:
  `` i18n(`cf_subtype_${sub_type}`) ``, `` i18n(`${entity_type}_delete_confirm_1`) ``. Whole
  **families** of keys live only dynamically and never appear as a literal anywhere.
- **PHP: ~15–20%** — `$i18n->get($var)`, array lookups.
- **Twig: ~20–30%** — `{{ var|i18n }}`.

So tree-sitter buys precision on the *easy* half (clean literals) where regex would already do fine
because the wrappers are unambiguous; it contributes **nothing** to the *hard* half. The real
quality of the tool is decided by how it treats dynamics — not by the parser.

### Design consequence: a "guard" layer is a first-class feature, not a footnote

Promote dynamic handling from "mark low confidence" to an explicit mechanism that runs for **every
language** (including the Twig regex path):

- When a call's first argument is not a plain literal, extract its **static fragments** and emit a
  guard: `Guard::Prefix("cf_subtype_")`, `Guard::Suffix("_delete_confirm_1")`,
  `Guard::Contains(...)`.
- A PO key is **kept (Alive)** if it has an exact literal match **OR** matches any guard.
- Bias hard toward "keep": for a cleanup tool, a false "dead" (deleting a key that is built
  dynamically) ships a **raw key into production** — a user-visible bug. Missing a truly-dead key
  costs nothing.

### Design consequence: report residual risk, never hide it

A call like `i18n($x)` with no static part is a **blind spot** — it produces no guard and no
literal. The tool must **count and report blind call sites per language** (`blind_count`) so the
reviewer knows how much of the surface could not be checked. Silent truncation of coverage is
unacceptable for a deletion-driving report.

## Output mode: ranked review list, not deletion

For the first use (one-off cleanup of `example_repo`, whose translations are managed in **Lokalise**
as the source of truth), the tool **does not delete keys and does not edit `.po` files**. It emits a
**ranked review list** so a human removes keys in Lokalise.

Add an output taxonomy beyond a flat "unused" list:

- **Dead** — no literal, no guard, not near any dynamic family. Highest confidence.
- **NearDynamic** — sits inside a prefix cluster where siblings are referenced dynamically. Flag
  "verify by hand" rather than recommend deletion.
- plus the **blind-spot summary** described above.

(Deletion / `.po` rewriting / Lokalise-API export can be a later mode behind a flag — explicitly
out of scope for v1.)

## Config implications (keep it general, prove it on example_repo)

The original `[functions]` block assumes free functions. Real wrappers need more, so generalize:

- **Method-call wrappers with a receiver constraint.** `example_repo` uses `$i18n->get('…')` /
  `$this->i18n->get('…')`. `get` alone is far too generic, so the config must allow anchoring a
  call to a receiver (e.g. `i18n`, `this.i18n`). Model translation calls as
  `{ kind: function|method|filter, name, receiver?, key_arg_index }` rather than a bare name list.
- **Twig filter form.** `example_repo` uses the `|i18n` filter, not `|trans` and not `{% trans %}`.
  The Twig patterns must be config-driven (filter name list), not hardcoded to `trans`.
- **JS wrapper.** `example_repo`'s wrapper is a plain `i18n('…')` (from
  `frontend/js/lib/utils/format/index.ts`), not `i18n.t`/`$t`. Confirms the function list must be
  per-project.

Example config we'll ship as the `example_repo` testbed:

```toml
[scan]
po_patterns = ["app/libs/locale/locales/*/LC_MESSAGES/messages.po"]
source_extensions = ["php", "twig", "js", "ts", "jsx", "tsx"]
ignore_dirs = ["vendor", "node_modules", "cache", "build"]

[[calls]]            # PHP method wrapper with receiver constraint
lang = "php"
kind = "method"
name = "get"
receiver = ["i18n", "this.i18n"]
key_arg_index = 0

[[calls]]            # JS/TS function wrapper
lang = "js"
kind = "function"
name = "i18n"
key_arg_index = 0

[[calls]]            # Twig filter
lang = "twig"
kind = "filter"
name = "i18n"

[output]
mode = "review"      # review | (future: delete, lokalise-export)
format = "text"      # text | json
fail_on = "dead"     # never | dead | dead-or-blind  (exit-code policy, Addendum 2 §8)
```

## Notes on this repo's PO reality (validation expectations, not hardcoding)

- ~17,000 keys per locale, 7 locales, **single domain** `messages`, **plural forms present**,
  **no `msgctxt`**. So the context/domain machinery from the original plan is correct to *support*
  generically, but we won't exercise it here. Build the key universe as the **union of msgids across
  all locales**.

## Updated v1 checklist (additive to the original)

In addition to the original v1 checklist:

- [ ] Generalized `[[calls]]` config: function / method (with receiver) / filter, configurable
      `key_arg_index`.
- [ ] First-class **guard layer** (prefix/suffix/contains) from non-literal arguments, applied
      across all languages.
- [ ] **Blind-spot counting** and a residual-risk summary in the report.
- [ ] **Review-list output mode** with `Dead` / `NearDynamic` buckets (no deletion in v1).
- [ ] Union-of-locales key index; skip obsolete (`#~`) and fuzzy entries.
- [ ] Ship `examples/example_repo.toml` and an end-to-end test that runs the crate against a small
      `example_repo`-shaped fixture (literal hit, dynamic-guard hit, blind call, truly-dead key).

## Verification (testbed = example_repo)

1. Unit fixtures: literal call per language → Alive; `` i18n(`cf_${x}`) `` + key `cf_subtype` →
   Alive via guard (not Dead); `i18n($x)` → increments `blind_count`; unreferenced key → Dead.
2. Run against real `example_repo`; eyeball top 15–20 `Dead` for obviously-live dynamic families.
3. Manual regression: confirm 5 known dynamic keys (`cf_subtype_*`, `*_delete_confirm_1`) land in
   Alive, not Dead.
4. `cargo test` green; the crate runs as a standalone CLI on any other repo by swapping the config.

---

# Addendum 2 — resolved design decisions

> Architecture review of this plan surfaced 8 gaps. This section is **authoritative** and supersedes
> any conflicting wording above (notably the old low/medium/high confidence scale). It is organized
> as a pipeline of small modules behind small interfaces:
>
> `PO index` → `literal decoding` → `extractor adapters` (per language/kind) → `liveness` → `reporter`.

## §1 Guard layer: no empty/trivial guards (the central correctness invariant)

From a non-literal key argument, extract the **maximal static fragments** of the template/concat and
emit guards:

- fragment at the start → `Guard::Prefix`, at the end → `Guard::Suffix`, interior → `Guard::Contains`;
- a fragment shorter than `min_guard_len` (default **3**, configurable) does **not** create a guard;
- if no fragment qualifies, the call site produces **no guard** and increments `blind_count`.

**Invariant:** a guard is never empty and never shorter than `min_guard_len`. This kills the landmine
where `` i18n(`${a}_${b}`) `` (prefix `""`) would otherwise mark the whole catalog Alive. Guard
matching is canonical-substring/prefix/suffix against the decoded msgid (see §4).

## §2 Result taxonomy: `Dead` / `Alive` + blind summary (NearDynamic dropped for v1)

Two real signals exist (literal-match, guard-match), so v1 has two buckets — no third, invented one:

- **Dead** — no literal match and no guard match. Recommended for review/removal.
- **Alive** — literal match **or** guard match.
- **blind summary** — per-language `blind_count` reported alongside, never hidden.

`NearDynamic` is deferred to v2 (it would need a prefix-clustering module and a third signal); revisit
only if the flat `Dead` list proves noisy in practice.

## §3 One classification vocabulary

The low/medium/high `--confidence` / `min_confidence` scale is **removed**. Live keys instead carry
`alive_via: literal | guard`. "Alive via guard" is exactly the old "low confidence", but expressed in
the single status system — one axis, not two overlapping ones.

## §4 Literal decoding module (new, named, shared)

A dedicated module turns a source literal into its **canonical runtime string** before any comparison:

- PHP `'…'` → unescape only `\\`, `\'`; PHP `"…"` → full escapes, **but if it contains `$`/`{$}`
  interpolation it is not a plain literal → route to the guard path (§1), not the literal set**;
- JS regular and `` `template` `` strings; Twig string literals;
- PO side: confirm `poparser` already returns decoded msgids; lock this with a unit test.

This module is also the single decision point for **"plain literal vs guard"**. Comparison is strictly
canonical-to-canonical (e.g. PHP `"a\nb"` and a PO `msgid "a\nb"` must both decode to a real newline).

## §5 Extractor = a set of per-kind matchers, not one query

Adapter interface: `extract(source) -> { literals: Set, guards: Vec<Guard>, blind: usize }`.
Each `[[calls]]` entry compiles the matcher for its `kind`:

- `function` → `function_call_expression`, match on name;
- `method` → `member_call_expression` / `scoped_call_expression`, match on name **and** receiver
  (`i18n`, `$this->i18n` → normalize the object node);
- `filter` (Twig) → regex on the filter name.

`key_arg_index` selects the argument node, which is handed to §4. This fixes the contradiction in the
original sample query (it only matched free functions, while `example_repo` needs the method wrapper
`$i18n->get('…')`).

## §6 tree-sitter concurrency

`tree_sitter::Parser` is **not `Sync`** and is costly to recreate per file. Design:

- parallelize over files with `rayon`; each worker holds **thread-local `Parser` instances (one per
  language)**, reused across files;
- `Language`/grammar objects are `'static` and shared; results collected in `DashMap`.

Pin the tree-sitter version. Grammar choice is **decided: `language_php()`** (not `language_php_only()`)
— `example_repo` sources mix PHP with HTML/Twig-ish markup, so the HTML-aware grammar is required.
Still ADR-worthy because the grammar crates have had ABI breaks across releases.

## §7 Deletion-safety boundary

Resolved scope: the `example_repo` `messages` catalog is **not consumed by other repos** (confirmed),
so v1 scans a single root. Still:

- `[scan] source_roots = ["."]` is modeled as a **list** from day one, so multi-repo coverage is a
  config change, not a code change;
- every report carries a one-line header: *"scope = the scanned source roots; external consumers
  (DB/config, other services, email/cron templates) are invisible — verify in Lokalise before
  deleting."*

## §8 CLI / exit-code semantics (CI-gate by default)

- `0` — ran successfully, no `Dead` keys;
- `1` — ran successfully, ≥1 `Dead` key (so CI fails red);
- `2` — error (no PO files found, bad config, parse failure).

Tunable via `[output] fail_on = never | dead | dead-or-blind` (default `dead`). JSON output mirrors the
same buckets and the blind summary for machine consumption.

## Updated v1 checklist (supersedes the confidence-related items above)

- [ ] Guard layer with `min_guard_len` invariant (§1); never emits empty/trivial guards.
- [ ] `Dead` / `Alive` taxonomy + `alive_via` tag + per-language blind summary (§2, §3).
- [ ] Literal-decoding module with PHP single/double-quote + interpolation routing (§4).
- [ ] Per-kind extractor adapters (function / method+receiver / filter) (§5).
- [ ] `rayon` + thread-local parsers; pinned tree-sitter version (§6).
- [ ] `source_roots` as a list + scope caveat in the report header (§7).
- [ ] Exit-code policy with `fail_on` (§8).
