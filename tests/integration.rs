//! Phase 10 — end-to-end integration over the *compiled binary*.
//!
//! These tests run the real `dead-poets` executable (the same one a user
//! invokes) against synthetic, `example_repo`-shaped fixtures. They assert the
//! full pipeline — config → PO universe → extraction → liveness → report → exit
//! code — not just a library unit.
//!
//! Two properties are pinned:
//!  1. The four liveness outcomes are produced together: a literal hit, a
//!     dynamic-guard hit, a blind site, and a truly-dead key.
//!  2. The engine is config-driven: a completely different repo shape (other
//!     call convention, other PO layout) works by swapping config alone.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the binary under test, provided by Cargo for integration tests.
const BIN: &str = env!("CARGO_BIN_EXE_dead-poets");

const PO_HEADER: &str = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n";

fn fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dead-poets-e2e-{tag}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
}

/// Run the binary `scan`; return (exit_code, parsed_json_stdout).
fn run_scan_json(root: &Path, config: &Path) -> (i32, serde_json::Value) {
    let output = Command::new(BIN)
        .arg("scan")
        .arg(root)
        .arg("--config")
        .arg(config)
        .arg("--format")
        .arg("json")
        .output()
        .expect("binary runs");
    let code = output.status.code().expect("process exited normally");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON ({e}):\n{stdout}"));
    (code, json)
}

/// True if any key object in `bucket` has the given msgid.
fn has_msgid(bucket: &serde_json::Value, msgid: &str) -> bool {
    bucket
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["msgid"] == msgid)
}

/// `alive_via` recorded for `msgid` in the alive bucket, if present.
fn alive_via<'a>(json: &'a serde_json::Value, msgid: &str) -> Option<&'a str> {
    json["alive"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["msgid"] == msgid)
        .and_then(|k| k["alive_via"].as_str())
}

/// The four outcomes in one run: literal hit, dynamic-guard hit, blind site,
/// and a truly-dead key — over the example_repo call convention (`i18n('…')`).
#[test]
fn example_repo_shape_produces_all_four_outcomes() {
    let dir = fixture_dir("four-outcomes");

    // PO universe: literal-used, guard-covered, index-used (locale['..']), orphan.
    write(
        &dir,
        "locale/en/LC_MESSAGES/messages.po",
        &format!(
            "{PO_HEADER}\
             msgid \"save_button\"\nmsgstr \"Save\"\n\n\
             msgid \"cf_subtype_red\"\nmsgstr \"Red\"\n\n\
             msgid \"Delete email\"\nmsgstr \"Delete\"\n\n\
             msgid \"totally_unused_key\"\nmsgstr \"Orphan\"\n"
        ),
    );

    // Source: literal call, dynamic template (guard Prefix), a blind call, and
    // an index access into a lang map (`locale['Delete email']`).
    write(
        &dir,
        "src/app.js",
        "i18n('save_button');\n\
         i18n(`cf_subtype_${kind}`);\n\
         i18n(varKey);\n\
         const m = locale['Delete email'];\n",
    );

    write(
        &dir,
        "dp.toml",
        "[scan]\n\
         po_patterns = [\"**/*.po\"]\n\
         source_extensions = [\"js\"]\n\
         \n\
         [[calls]]\n\
         lang = \"js\"\n\
         kind = \"function\"\n\
         name = \"i18n\"\n\
         \n\
         [[calls]]\n\
         lang = \"js\"\n\
         kind = \"index\"\n\
         name = \"locale\"\n\
         \n\
         [output]\n\
         fail_on = \"dead\"\n",
    );

    let (code, json) = run_scan_json(&dir, &dir.join("dp.toml"));

    // literal hit
    assert_eq!(alive_via(&json, "save_button"), Some("literal"));
    // dynamic-guard hit: `cf_subtype_${kind}` -> Prefix("cf_subtype_") matches
    assert_eq!(alive_via(&json, "cf_subtype_red"), Some("guard"));
    // index hit: locale['Delete email'] keeps the key alive as a literal
    assert_eq!(alive_via(&json, "Delete email"), Some("literal"));
    // truly-dead key
    assert!(has_msgid(&json["dead"], "totally_unused_key"));
    assert!(!has_msgid(&json["alive"], "totally_unused_key"));
    // blind site: i18n(varKey)
    assert_eq!(json["blind"]["js"], 1);
    // a dead key exists and fail_on=dead -> exit 1
    assert_eq!(json["summary"]["dead"], 1);
    assert_eq!(code, 1, "dead present under fail_on=dead -> exit 1");

    std::fs::remove_dir_all(&dir).ok();
}

/// Run the binary `scan --audit`; return (exit_code, parsed_json_stdout).
fn run_scan_audit_json(root: &Path, config: &Path) -> (i32, serde_json::Value) {
    let output = Command::new(BIN)
        .arg("scan")
        .arg(root)
        .arg("--config")
        .arg(config)
        .arg("--format")
        .arg("json")
        .arg("--audit")
        .output()
        .expect("binary runs");
    let code = output.status.code().expect("process exited normally");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON ({e}):\n{stdout}"));
    (code, json)
}

/// `--audit` buckets the Dead list by residual source trace (substring /
/// skeleton / none) and is advisory: classification and exit code are unchanged.
#[test]
fn audit_scores_dead_bucket_without_changing_exit_code() {
    let dir = fixture_dir("audit");

    // Three dead keys, one of each trace tier, plus a live one.
    write(
        &dir,
        "locale/en/LC_MESSAGES/messages.po",
        &format!(
            "{PO_HEADER}\
             msgid \"used_lit\"\nmsgstr \"x\"\n\n\
             msgid \"Ghost phrase here\"\nmsgstr \"x\"\n\n\
             msgid \"Deleted %d rows\"\nmsgstr \"x\"\n\n\
             msgid \"Nowhere at all\"\nmsgstr \"x\"\n"
        ),
    );

    // - used_lit: real call -> alive (out of the dead bucket).
    // - "Ghost phrase here": only in a comment -> substring (not an AST literal,
    //   so it is dead, but raw grep finds it).
    // - "Deleted %d rows": assembled by concatenation -> skeleton (fragments
    //   'Deleted ' and ' rows' present; the full msgid is not).
    // - "Nowhere at all": absent -> no trace.
    write(
        &dir,
        "src/app.js",
        "i18n('used_lit');\n\
         // Ghost phrase here is only mentioned in this comment\n\
         const s = 'Deleted ' + n + ' rows';\n",
    );

    write(
        &dir,
        "dp.toml",
        "[scan]\n\
         po_patterns = [\"**/*.po\"]\n\
         source_extensions = [\"js\"]\n\
         \n\
         [[calls]]\n\
         lang = \"js\"\n\
         kind = \"function\"\n\
         name = \"i18n\"\n\
         \n\
         [output]\n\
         fail_on = \"dead\"\n",
    );

    let cfg = dir.join("dp.toml");
    let (plain_code, plain_json) = run_scan_json(&dir, &cfg);
    let (audit_code, audit_json) = run_scan_audit_json(&dir, &cfg);

    // Classification and exit code are identical with and without --audit.
    assert_eq!(plain_code, audit_code, "audit must not change the exit code");
    assert_eq!(plain_json["summary"]["dead"], audit_json["summary"]["dead"]);
    assert!(plain_json.get("audit").is_none(), "no audit object without the flag");

    // 3 dead keys, one per tier.
    let audit = &audit_json["audit"];
    assert_eq!(audit["dead_total"], 3);
    assert_eq!(audit["substring"], 1);
    assert_eq!(audit["skeleton"], 1);
    assert_eq!(audit["no_trace"], 1);

    // The recheck list names the traced keys with their tier; the no-trace key
    // is omitted (it stays in the actionable Dead list).
    let traced = audit["traced"].as_array().unwrap();
    let tier = |id: &str| {
        traced
            .iter()
            .find(|t| t["msgid"] == id)
            .map(|t| t["trace"].as_str().unwrap())
    };
    assert_eq!(tier("Ghost phrase here"), Some("substring"));
    assert_eq!(tier("Deleted %d rows"), Some("skeleton"));
    assert_eq!(tier("Nowhere at all"), None);

    std::fs::remove_dir_all(&dir).ok();
}

/// Standalone proof: the *same binary* handles a different repo with a different
/// call convention (PHP `$lang->tr('key')`) and a different PO layout, with no
/// code changes — only config differs. No example_repo specifics are baked in.
#[test]
fn different_repo_works_by_swapping_config_only() {
    let dir = fixture_dir("standalone");

    write(
        &dir,
        "i18n/messages.po",
        &format!("{PO_HEADER}msgid \"greeting\"\nmsgstr \"Hello\"\n"),
    );
    write(&dir, "lib/page.php", "<?php $lang->tr('greeting'); ?>\n");

    write(
        &dir,
        "other.toml",
        "[scan]\n\
         po_patterns = [\"**/*.po\"]\n\
         source_extensions = [\"php\"]\n\
         \n\
         [[calls]]\n\
         lang = \"php\"\n\
         kind = \"method\"\n\
         name = \"tr\"\n\
         receiver = [\"lang\"]\n\
         \n\
         [output]\n\
         fail_on = \"dead\"\n",
    );

    let (code, json) = run_scan_json(&dir, &dir.join("other.toml"));

    assert_eq!(alive_via(&json, "greeting"), Some("literal"));
    assert_eq!(json["summary"]["dead"], 0);
    assert_eq!(code, 0, "no dead keys -> exit 0");

    std::fs::remove_dir_all(&dir).ok();
}

/// The shipped example_repo.toml deserializes with the real config loader —
/// it must never rot into something the engine can't parse.
#[test]
fn shipped_example_config_loads() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let cfg_path = Path::new(manifest).join("examples/example_repo.toml");
    let cfg = dead_poets::config::Config::load(&cfg_path).expect("example config loads");

    // The three documented conventions are present.
    assert!(cfg.calls.iter().any(|c| c.lang == "php"
        && c.name == "get"
        && c.receiver.as_deref().is_some_and(|r| r.contains(&"i18n".to_string()))));
    assert!(cfg.calls.iter().any(|c| c.lang == "js" && c.name == "i18n"));
    assert!(cfg.calls.iter().any(|c| c.lang == "twig" && c.name == "i18n"));
}
