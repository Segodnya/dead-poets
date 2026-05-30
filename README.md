# dead-poets

[![CI](https://github.com/Segodnya/dead-poets/actions/workflows/ci.yml/badge.svg)](https://github.com/Segodnya/dead-poets/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A playful nod to *Dead Poets Society* — here **dead** means dead (unused) keys
hidden in your gettext PO catalogs.

Find **dead** (unused) gettext keys: msgids in your `.po`/`.pot` catalogs that no
source ever references. Polyglot — PHP, Twig, JS/TS — via tree-sitter AST parsing
(regex for Twig). Everything project-specific lives in config, so pointing it at a
new repo is a config change, not a code change.

## Install

```sh
cargo build --release   # binary at target/release/dead-poets
```

## Usage

```sh
dead-poets scan <path> -c dead-poets.toml [-f text|json] [-v] [--audit] \
  [--max-dead N | --max-dead-ratio R]
```

- `path` — project root (default `.`).
- `-c, --config` — config file (default `dead-poets.toml`).
- `-f, --format` — `text` (colored) or `json` (CI).
- `-v, --verbose` — repeatable (`-v`, `-vv`, ...).
- `--audit` — trust score for the Dead bucket (see below); advisory.
- `--max-dead N` / `--max-dead-ratio R` — dead-key budget (see below); override
  config, mutually exclusive.

Exit codes: `0` within budget · `1` over budget (CI fails) · `2` operational
error. Gate policy: `[output] fail_on = never | dead | dead-or-blind` (default
`dead`).

## Dead-key budget (ratchet)

A real catalog starts with thousands of dead keys, so failing on *any* is
unusable. Set a budget and lower it as you delete — a debt ceiling:

```toml
[output]
max_dead = 2900          # fail when dead > 2900
# max_dead_ratio = 0.15  # …or dead / total > 15% (mutually exclusive)
```

Default (`max_dead = 0`) fails on any dead key. The budget relaxes only the Dead
gate — `dead-or-blind` still fails on blind sites. The report prints a budget line
(and a `budget` object in JSON) with the remaining headroom.

## Classification

Universe = union of msgids across all catalogs (obsolete `#~` / fuzzy skipped).
Each key is:

- **Alive** — referenced, with `alive_via`: `literal` (`i18n('save_button')`),
  `guard` (a static fragment of a dynamic key — `` i18n(`cf_subtype_${x}`) `` keeps
  every `cf_subtype_*` alive; fragments shorter than `min_guard_len` are ignored),
  or `whitelist` (runtime-only keys).
- **Suspect** — no modeled call, but the msgid appears verbatim as a source literal
  (data table / enum dispatched dynamically). Review hint; exit-neutral.
- **Dead** — no reference of any kind.

The tool **biases toward keep**: a false "dead" ships a raw key to prod, while a
missed dead key costs nothing. A call with no static part (`i18n($x)`) is a **blind
spot** — counted per language, always reported.

## Auditing the Dead bucket

`--audit` greps each dead msgid against **raw source** (comments, HTML, heredocs,
substrings — not just AST literals) and tiers it:

- **substring** — the full msgid occurs verbatim.
- **skeleton** — placeholder keys (`%s`/`%d`/`{0}`) whose static fragments all
  appear (the `sprintf`-assembled tell).
- **none** — no trace: high-confidence dead.

Advisory and case-sensitive: never reclassifies or changes the exit code, and runs
only with `--audit` (zero default overhead). JSON gains an `audit` object.

## Configuration

```toml
[scan]
po_patterns = ["**/*.po", "**/*.pot"]
source_extensions = ["php", "twig", "js", "ts", "jsx", "tsx"]
ignore_dirs = ["vendor", "node_modules", "cache", "var"]   # on top of .gitignore
source_roots = ["."]                                       # list → multi-repo

# One [[calls]] block per translation convention.
[[calls]]
lang = "php"
kind = "method"               # function | method | filter | index
name = "get"                  # call/filter name, or indexed object for `index`
receiver = ["i18n", "this.i18n"]   # method only: allowed receivers
key_arg_index = 0             # which arg holds the key (0-based)

[whitelist]
file = "i18n-whitelist.txt"   # one key per line, # comments allowed
keys = ["dynamic.key1"]       # or inline

[output]
format = "text"               # text | json (overridden by -f)
fail_on = "dead"              # never | dead | dead-or-blind
min_guard_len = 3
max_dead = 2900               # optional budget (absolute)
# max_dead_ratio = 0.15       # …or a share of the universe
```

**Call kinds:** `function` (`i18n('key')`) · `method` with `receiver`
(`$i18n->get('key')`; `$`/`->`/`.` normalized away) · `filter` (`{{ 'key'|i18n }}`)
· `index` (`locale['key']`).

## Scope & safety

Keys are **Dead** only relative to the scanned roots. External consumers —
databases, other repos sharing the catalog, email/cron templates — are invisible,
so a `Dead` key may still be used. **Never auto-delete:** verify each key in your
TMS first.

## License

[Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
