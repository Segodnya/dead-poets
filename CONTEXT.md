# CONTEXT — dead-poets domain & architecture glossary

Shared language for the crate. Domain terms describe *what* dead-poets reasons
about; architecture terms name the *seams* it reasons through. Most domain terms
have their authoritative definition in the relevant module's docstring — this file
is the index and the cross-module vocabulary.

## Domain

- **Universe** — the deduplicated union of msgids across all PO/POT catalogs; the
  set of keys checked for liveness (`po`). Obsolete (`#~`) and fuzzy entries are
  excluded.
- **Alive / Suspect / Dead** — a key's liveness status (`liveness`). Alive carries
  an **alive_via**: `literal`, `guard`, or `whitelist`. Suspect = msgid appears
  verbatim as a source literal but no modeled call references it. Dead = no
  reference of any kind.
- **Guard** — a static fragment of a dynamic key (`i18n(`cf_${x}`)` → prefix
  `cf_`) that keeps every matching key Alive (`guard`). Never shorter than
  `min_guard_len`.
- **Blind spot** — a call site with no resolvable static fragment (`i18n($x)`).
  Counted per language family, never hidden.
- **CallSpec** — one `[[calls]]` convention: a `(lang, kind, name, receiver?,
  key_arg_index)` descriptor of a translation call site (`config`). Kinds:
  `function`, `method`, `filter`, `index`.
- **Trace tier** — the `--audit` trust score over the Dead bucket: `substring`,
  `skeleton`, or `none` (`audit`). Advisory; never reclassifies.
- **Budget (ratchet)** — the dead-key debt ceiling that relaxes the Dead gate
  (`budget`). Absolute (`Count`) XOR ratio (`Ratio`); CLI overrides config.

## Architecture seams

- **AstMatcher** — the per-language adapter seam in `extract` (`Php`, `Js`). Each
  adapter exposes only what varies per grammar: `classify` a node into a
  **CallShape**, `segments` (decode a value node), and `literal` (recognise a
  string). Twig is *not* an AstMatcher — it has no tree and stays a regex adapter.
- **CallShape** — a classified call site (`Function` / `Method` / `Index`) carrying
  the name, normalized receiver, and argument/subscript nodes. The driver matches
  it against the `[[calls]]` specs (`match_spec`, the one invariant) and records
  the key argument. This is the seam that makes "how is PHP matched" local to one
  adapter and individually testable.
- **walk::find_files** — the single file-discovery seam shared by source scanning,
  PO collection, and the audit pass. Owns the walk policy: `.gitignore` +
  `ignore_dirs` pruning, cross-root dedup by absolute path, and the deterministic
  sort the parallel folds depend on. Callers pass an `accept` over a **FileEntry**
  `{root, rel, abs}` and express only what counts as a hit.
