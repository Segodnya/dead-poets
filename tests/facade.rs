//! End-to-end integration over the *library facade* [`dead_poets::run`].
//!
//! Complements `integration.rs` (which drives the compiled binary): this proves
//! the same pipeline — config → PO universe → extraction → liveness → audit — is
//! reachable by a library consumer through the **public API only**, with no CLI,
//! no rendering, and no exit-code mapping. It links against the crate exactly as
//! an external dependency would, so it pins the facade's public surface.

use std::path::{Path, PathBuf};

use dead_poets::audit::Trace;
use dead_poets::liveness::{AliveVia, Status};
use dead_poets::run;

const PO_HEADER: &str = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n";

fn fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dead-poets-facade-{tag}"));
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

/// Status recorded for `msgid` in the facade's report.
fn status_of(out: &dead_poets::Outcome, msgid: &str) -> Status {
    out.report
        .verdicts
        .iter()
        .find(|v| v.key.msgid == msgid)
        .unwrap_or_else(|| panic!("no verdict for {msgid}"))
        .status
}

/// All five outcomes through the public facade, plus the audit pass, over one
/// fixture: a literal hit, a dynamic-guard hit, a suspect (bare source literal),
/// a substring-traced dead key, and a truly-dead key — with a blind site too.
#[test]
fn facade_run_exposes_every_bucket() {
    let dir = fixture_dir("buckets");

    write(
        &dir,
        "locale/en/LC_MESSAGES/messages.po",
        &format!(
            "{PO_HEADER}\
             msgid \"save_button\"\nmsgstr \"Save\"\n\n\
             msgid \"cf_subtype_red\"\nmsgstr \"Red\"\n\n\
             msgid \"data_key\"\nmsgstr \"Data\"\n\n\
             msgid \"Ghost in comment\"\nmsgstr \"Ghost\"\n\n\
             msgid \"orphan_key\"\nmsgstr \"Orphan\"\n"
        ),
    );

    // - save_button: real call            -> Alive(Literal)
    // - cf_subtype_${k}: dynamic template -> Alive(Guard) covers cf_subtype_red
    // - 'data_key': bare literal, no call -> Suspect
    // - varKey: no static fragment        -> blind site
    // - "Ghost in comment": comment only  -> Dead, raw-grep substring trace
    // - orphan_key: nowhere               -> Dead, no trace
    write(
        &dir,
        "src/app.js",
        "i18n('save_button');\n\
         i18n(`cf_subtype_${k}`);\n\
         const x = 'data_key';\n\
         i18n(varKey);\n\
         // Ghost in comment is only mentioned here\n",
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

    let cfg_path = dir.join("dp.toml");

    // Without audit: classification buckets, blind summary, no audit object.
    let out = run(&cfg_path, &dir, false).expect("facade runs");

    assert_eq!(
        status_of(&out, "save_button"),
        Status::Alive(AliveVia::Literal)
    );
    assert_eq!(
        status_of(&out, "cf_subtype_red"),
        Status::Alive(AliveVia::Guard)
    );
    assert_eq!(status_of(&out, "data_key"), Status::Suspect);
    assert_eq!(status_of(&out, "Ghost in comment"), Status::Dead);
    assert_eq!(status_of(&out, "orphan_key"), Status::Dead);

    assert_eq!(out.report.alive_count(), 2);
    assert_eq!(out.report.suspect_count(), 1);
    assert_eq!(out.report.dead_count(), 2);
    assert_eq!(
        out.report.total_blind(),
        1,
        "i18n(varKey) is one blind site"
    );
    assert_eq!(out.report.blind.get("js"), Some(&1));
    assert!(out.audit.is_none(), "no audit object without the flag");

    // With audit: the two dead keys are tiered; classification is unchanged.
    let audited = run(&cfg_path, &dir, true).expect("facade runs with audit");
    assert_eq!(audited.report.dead_count(), 2, "audit does not reclassify");

    let audit = audited.audit.expect("run_audit populates the audit");
    assert_eq!(audit.dead_total, 2);
    assert_eq!(
        audit.substring, 1,
        "\"Ghost in comment\" left a substring trace"
    );
    assert_eq!(audit.no_trace, 1, "orphan_key left no trace");
    assert_eq!(audit.skeleton, 0);

    let traced_tier = |id: &str| {
        audit
            .traced
            .iter()
            .find(|(k, _)| k.msgid == id)
            .map(|(_, t)| *t)
    };
    assert_eq!(traced_tier("Ghost in comment"), Some(Trace::Substring));
    assert_eq!(traced_tier("orphan_key"), None, "no-trace keys are omitted");

    std::fs::remove_dir_all(&dir).ok();
}
