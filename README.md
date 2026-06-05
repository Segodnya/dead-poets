# dead-poets

[![CI](https://github.com/Segodnya/dead-poets/actions/workflows/ci.yml/badge.svg)](https://github.com/Segodnya/dead-poets/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A playful nod to *Dead Poets Society* — here **dead** means unused: msgids in your
gettext `.po`/`.pot` catalogs that no source references. Polyglot — PHP, Twig,
JS/TS via tree-sitter AST (regex for Twig). Project specifics live in config, so a
new repo is a config change, not a code change.

> **Scope:** the key universe is **gettext `.po`/`.pot` only** — the *reference*
> side is polyglot, the *catalog* side is not. Reading JSON/YAML/`.properties`/
> `.arb` catalogs (react-i18next, vue-i18n, Rails, Flutter ARB, Android/iOS) is a
> non-goal by design. Keys in `en.json`? Wrong tool.

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
- `--max-dead N` / `--max-dead-ratio R` — dead-key budget; override config,
  mutually exclusive.

Exit codes: `0` within budget · `1` over budget (CI fails) · `2` operational
error. Gate: `[output] fail_on = never | dead | dead-or-blind` (default `dead`).

## Dead-key budget (ratchet)

Catalogs start with thousands of dead keys, so failing on *any* is unusable. Set a
debt ceiling and lower it as you delete:

```toml
[output]
max_dead = 2900          # fail when dead > 2900
# max_dead_ratio = 0.15  # …or dead / total > 15% (mutually exclusive)
```

Default (`max_dead = 0`) fails on any dead key. The budget relaxes only the Dead
gate (`dead-or-blind` still fails on blind sites) and prints a budget line — a
`budget` object in JSON — with remaining headroom.

## Classification

Universe = union of msgids across all catalogs (obsolete `#~` / fuzzy skipped).
Each key is:

- **Alive** — referenced, with `alive_via`: `literal` (`i18n('save_button')`),
  `guard` (a static fragment of a dynamic key — `` i18n(`cf_subtype_${x}`) `` keeps
  every `cf_subtype_*` alive; fragments shorter than `[guard] min_len` are ignored),
  or `whitelist` (runtime-only keys).
- **Suspect** — no modeled call, but the msgid appears verbatim as a source literal
  (data table / enum dispatched dynamically). Review hint; exit-neutral.
- **Dead** — no reference of any kind.

The tool **biases toward keep**: a false *dead* ships a raw key to prod, a missed
dead key costs nothing. A call with no static part (`i18n($x)`) is a **blind
spot** — counted per language, always reported.

## Auditing the Dead bucket

`--audit` greps each dead msgid against **raw source** (comments, HTML, heredocs,
substrings — not just AST literals) and tiers it:

- **substring** — the full msgid occurs verbatim.
- **skeleton** — placeholder keys (`%s`/`%d`/`{0}`) whose static fragments all
  appear (the `sprintf`-assembled tell).
- **none** — no trace: high-confidence dead.

Advisory and case-sensitive: never reclassifies or changes the exit code, runs only
with `--audit` (zero default overhead). JSON gains an `audit` object.

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

[guard]
min_len = 3                   # shortest static fragment that forms a guard

[output]
format = "text"               # text | json (overridden by -f)
fail_on = "dead"              # never | dead | dead-or-blind
max_dead = 2900               # optional budget (absolute)
# max_dead_ratio = 0.15       # …or a share of the universe
```

**Call kinds:** `function` (`i18n('key')`) · `method` with `receiver`
(`$i18n->get('key')`; `$`/`->`/`.` normalized away) · `filter` (`{{ 'key'|i18n }}`)
· `index` (`locale['key']`).

> Migrating from ≤ 0.2: `min_guard_len` moved from `[output]` to `[guard] min_len`
> — an old `[output] min_guard_len` is now rejected, not silently ignored.

## Library use

`dead_poets::run` is the whole pipeline in one call — config path in,
classification out:

```rust
use std::path::Path;
use dead_poets::run;

let outcome = run(Path::new("dead-poets.toml"), Path::new("."), false)?; // false = no audit

println!("{} dead of {} keys", outcome.report.dead_count(), outcome.report.verdicts.len());
for verdict in outcome.report.dead() {
    println!("dead: {}", verdict.key.msgid);
}
```

`Outcome` carries the loaded `config`, the `report` (verdicts + blind summary), and
an optional `audit`; the budget (`budget::resolve` + `DeadBudget::is_exceeded`) and
rendering stay caller-side. Disable the default `cli` feature to drop the terminal
deps (clap / colored / env_logger):

```toml
[dependencies]
dead-poets = { version = "0.3", default-features = false }  # classifier only
```

## Scope & safety

Keys are **Dead** only relative to the scanned roots. External consumers —
databases, other repos sharing the catalog, email/cron templates — are invisible,
so a `Dead` key may still be used. **Never auto-delete:** verify each key in your
TMS first.

## License

[Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
