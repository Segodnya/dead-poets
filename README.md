# dead-poets

[![CI](https://github.com/Segodnya/dead-poets/actions/workflows/ci.yml/badge.svg)](https://github.com/Segodnya/dead-poets/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A playful nod to *Dead Poets Society* — here **dead** means dead (unused) keys
hidden in your gettext PO catalogs.

`dead-poets` is a standalone Rust CLI (and reusable library) that finds gettext
keys present in your `.po`/`.pot` files but never referenced from source. It
understands a polyglot codebase — PHP, Twig, JS/TS — via real AST parsing
(tree-sitter) for PHP/JS/TS and a regex scanner for Twig.

Everything project-specific lives in a config file; the engine knows nothing
about any particular repository. Pointing it at a new repo is a config change,
not a code change.

## Install

```sh
cargo build --release
# binary at target/release/dead-poets
```

## Usage

```sh
dead-poets scan <path> --config dead-poets.toml [--format text|json] [-v] [--audit]
```

- `path` — project root to scan (default `.`).
- `--config, -c` — config file (default `dead-poets.toml`).
- `--format, -f` — `text` (colored review list) or `json` (for CI).
- `--verbose, -v` — repeatable (`-v`, `-vv`, ...).
- `--audit` — score the Dead bucket against raw source (see *Auditing the Dead
  bucket* below). Advisory: it never changes classification or the exit code.

Exit codes (CI gate by default):

| code | meaning |
|------|---------|
| `0` | ran successfully, no dead keys |
| `1` | ran successfully, ≥1 dead key (CI fails red) |
| `2` | operational error (no PO files, bad config, parse failure) |

Tune via `[output] fail_on = never | dead | dead-or-blind` (default `dead`).

## How it classifies a key

The key universe is the **union of msgids** across every matched PO catalog
(obsolete `#~` and fuzzy entries skipped). Each key gets one status:

- **Alive** — referenced from source, with `alive_via`:
  - `literal` — an exact string-literal call, e.g. `i18n('save_button')`.
  - `guard` — matches a static fragment extracted from a *dynamic* key. From
    `` i18n(`cf_subtype_${x}`) `` we emit `Guard::Prefix("cf_subtype_")`, which
    keeps every `cf_subtype_*` key alive. Fragments shorter than `min_guard_len`
    (default `3`) never form a guard — that would keep the whole catalog.
  - `whitelist` — listed in `[whitelist]` (keys only resolvable at runtime:
    DB/config/external).
- **Suspect** — not referenced by any modeled call, but the msgid appears
  verbatim as a string literal *somewhere* in source (a data table, enum, or
  config array dispatched dynamically, e.g. `LANG_TYPE => 'Count of income
  calls'`). Not a confirmed use, so not Alive — but not Dead either. A review
  hint; exit-neutral (never fails CI).
- **Dead** — no reference of any kind. The removal-review list.

Dynamics dominate real codebases and an AST can't expand them, so the tool
**biases hard toward keep**: a false "dead" would ship a raw key to production,
while missing a truly-dead key costs nothing.

A call with no static part at all (`i18n($x)`) is a **blind spot** — it can't be
verified. Blind sites are counted per language and always reported, never hidden.

## Auditing the Dead bucket

`--audit` answers a separate question: *how much do you trust the Dead list?* It
greps every dead msgid against the **raw source** (not just AST string literals —
also comments, HTML text, heredocs, and substrings of larger strings) and buckets
each dead key by the strongest residual trace it leaves:

- **substring** — the full msgid occurs verbatim somewhere in source.
- **skeleton** — only for keys carrying `%s`/`%d`/`%1$s`/`{0}` placeholders, and
  only when there is no substring hit: stripping the placeholders yields static
  fragments (each ≥ `min_guard_len`) that *all* appear in source — the mark of a
  `sprintf`-assembled key.
- **none** — no trace of any kind: high-confidence dead.

The pass is **advisory** — it never reclassifies a key and never affects the exit
code; it just prints a trust line (and, in `--format json`, an `audit` object
listing the traced keys to recheck). Matching is case-sensitive. The pass only
runs when `--audit` is given, so the default scan carries zero overhead.

## Configuration

```toml
[scan]
po_patterns = ["**/*.po", "**/*.pot"]            # catalogs (recursive globs)
source_extensions = ["php", "twig", "js", "ts", "jsx", "tsx"]
ignore_dirs = ["vendor", "node_modules", "cache", "var"]   # on top of .gitignore
source_roots = ["."]                              # a list → multi-repo is config-only

# Translation call sites. One [[calls]] block per convention.
[[calls]]
lang = "php"
kind = "method"               # function | method | filter | index
name = "get"                  # call/filter name, or indexed object for `index`
receiver = ["i18n", "this.i18n", "this._i18n"]   # method: allowed receivers
key_arg_index = 0             # which argument holds the key (0-based)

[[calls]]
lang = "js"
kind = "function"
name = "i18n"

[[calls]]
lang = "twig"
kind = "filter"
name = "i18n"                 # {{ 'key'|i18n }}

[[calls]]
lang = "js"
kind = "index"               # locale['Some Key'] — index access into a lang map
name = "locale"              # the indexed object; the subscript string is the key

[whitelist]
file = "i18n-whitelist.txt"  # one key per line, # comments allowed
keys = ["dynamic.key1"]      # or inline

[output]
mode = "review"              # review (only mode in v1)
format = "text"              # text | json (overridden by --format)
fail_on = "dead"             # never | dead | dead-or-blind
min_guard_len = 3
```

**Call kinds:**

- `function` — free function, `i18n('key')`.
- `method` — method with a `receiver` constraint, `$i18n->get('key')`. `$` and
  `->`/`.` are normalized away (`$this->i18n` → `this.i18n`); a factory-call
  receiver normalizes to its callee + `()` (`Container::get_i18n()` →
  `get_i18n()`).
- `filter` — Twig filter, `{{ 'key'|i18n }}`.
- `index` — bracket access into a translation map, `locale['key']`.

## Scope & safety

dead-poets reports keys as **Dead** only with respect to the **source roots it
scans**. External consumers are invisible to static analysis: databases/configs,
other services or repositories sharing the same catalog, and email/cron/generated
templates. A key marked `Dead` may still be used outside the scanned code.

**Never auto-delete from this report.** It is a ranked review list — verify each
key (e.g. in your TMS, the source of truth) before removing it.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
