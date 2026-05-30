//! Reporter — render the liveness result and decide the process exit code
//! (PLAN Addendum 2 §7, §8).
//!
//! Two formats mirror the same buckets:
//! - **text**: a one-line scope caveat header, the ranked `Dead` list (colored),
//!   the `Suspect` list (literal present but no modeled call), the per-language
//!   blind summary, and a count line.
//! - **json**: `dead` / `suspect` / `alive` (with `alive_via`) / `blind`.
//!
//! Exit policy is `fail_on`: `never` → 0; `dead` → 1 if any Dead; `dead-or-blind`
//! → 1 if any Dead or any blind site. `Suspect` is exit-neutral by design — it is
//! a review hint, not a failure. Code `2` (operational error) is owned by the
//! CLI, not this module.

use std::collections::BTreeMap;

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use crate::audit::{AuditReport, Trace};
use crate::budget::DeadBudget;
use crate::config::{FailOn, OutputFormat};
use crate::liveness::{AliveVia, LivenessReport, Status};
use crate::po::PoKey;

/// One-line caveat printed on every report: static analysis can't see external
/// consumers, so a `Dead` key may still be used outside the scanned roots.
pub const SCOPE_CAVEAT: &str = "scope = scanned source roots only; external consumers (DB/config, other services, email/cron templates) are invisible — verify in your TMS before deleting.";

fn alive_via_str(via: AliveVia) -> &'static str {
    match via {
        AliveVia::Literal => "literal",
        AliveVia::Guard => "guard",
        AliveVia::Whitelist => "whitelist",
    }
}

/// Dead keys sorted for stable, reviewable output.
fn sorted_dead(report: &LivenessReport) -> Vec<&PoKey> {
    let mut dead: Vec<&PoKey> = report.dead().map(|v| &v.key).collect();
    dead.sort_by(|a, b| (&a.msgctxt, &a.msgid).cmp(&(&b.msgctxt, &b.msgid)));
    dead
}

/// Suspect keys sorted for stable output.
fn sorted_suspect(report: &LivenessReport) -> Vec<&PoKey> {
    let mut suspect: Vec<&PoKey> = report.suspect().map(|v| &v.key).collect();
    suspect.sort_by(|a, b| (&a.msgctxt, &a.msgid).cmp(&(&b.msgctxt, &b.msgid)));
    suspect
}

/// Render a key for display: `[ctxt] msgid (+ plural)`.
fn display_key(key: &PoKey) -> String {
    let mut s = String::new();
    if let Some(ctxt) = &key.msgctxt {
        s.push_str(&format!("[{ctxt}] "));
    }
    s.push_str(&key.msgid);
    if let Some(plural) = &key.msgid_plural {
        s.push_str(&format!("  / {plural}"));
    }
    s
}

fn blind_line(blind: &BTreeMap<String, usize>) -> String {
    if blind.is_empty() {
        return "Blind spots: none".to_string();
    }
    let parts: Vec<String> = blind.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!(
        "Blind spots (unverifiable call sites): {}",
        parts.join(", ")
    )
}

fn trace_str(trace: Trace) -> &'static str {
    match trace {
        Trace::Substring => "substring",
        Trace::Skeleton => "skeleton",
        Trace::None => "none",
    }
}

/// The advisory dead-bucket trust block (text). One headline number plus a
/// pointer to the full recheck list, which lives in the JSON output.
fn audit_block(audit: &AuditReport) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Audit (dead-bucket trust): {} dead — {} no trace (high-confidence), {} substring, {} skeleton — recheck before deleting.\n",
        audit.dead_total, audit.no_trace, audit.substring, audit.skeleton,
    ));
    if !audit.traced.is_empty() {
        s.push_str(
            &format!(
                "  {} dead keys still trace to source — see --format json for the list.",
                audit.traced.len()
            )
            .dimmed()
            .to_string(),
        );
        s.push('\n');
    }
    s
}

/// The dead-key budget line (text). Shown only for a non-default budget; reports
/// the headroom (or overage) so the ratchet is visible at a glance.
fn budget_line(dead: usize, total: usize, budget: DeadBudget) -> Option<String> {
    if !budget.is_set() {
        return None;
    }
    let over = budget.is_exceeded(dead, total);
    let body = match budget {
        DeadBudget::Count(max) => {
            if over {
                format!(
                    "Budget: {dead} / {max} dead allowed — OVER by {}.",
                    dead - max
                )
            } else {
                format!(
                    "Budget: {dead} / {max} dead allowed — within budget ({} headroom).",
                    max - dead
                )
            }
        }
        DeadBudget::Ratio(r) => {
            let pct = if total > 0 {
                dead as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            let cap = r * 100.0;
            if over {
                format!("Budget: {pct:.1}% / {cap:.1}% dead ratio — OVER.")
            } else {
                format!("Budget: {pct:.1}% / {cap:.1}% dead ratio — within budget.")
            }
        }
    };
    Some(if over {
        body.red().to_string()
    } else {
        body.green().to_string()
    })
}

/// Render the text report.
pub fn render_text(
    report: &LivenessReport,
    audit: Option<&AuditReport>,
    budget: DeadBudget,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {}\n\n",
        "dead-poets".bold(),
        SCOPE_CAVEAT.dimmed()
    ));

    let dead = sorted_dead(report);
    if dead.is_empty() {
        out.push_str(&"No dead keys found.\n".green().to_string());
    } else {
        out.push_str(&format!(
            "{}\n",
            format!("Dead keys ({}):", dead.len()).bold()
        ));
        for key in &dead {
            out.push_str(&format!("  {}\n", display_key(key).red()));
        }
    }

    let suspect = sorted_suspect(report);
    if !suspect.is_empty() {
        out.push('\n');
        out.push_str(&format!(
            "{} {}\n",
            format!("Suspect ({}):", suspect.len()).bold(),
            "literal present in source but no modeled call — verify, don't delete".dimmed(),
        ));
        for key in &suspect {
            out.push_str(&format!("  {}\n", display_key(key).yellow()));
        }
    }

    out.push('\n');
    out.push_str(&blind_line(&report.blind));
    out.push('\n');
    out.push_str(&format!(
        "Summary: {} keys, {} alive, {} suspect, {} dead, {} blind\n",
        report.verdicts.len(),
        report.alive_count(),
        report.suspect_count(),
        report.dead_count(),
        report.total_blind(),
    ));

    if let Some(line) = budget_line(report.dead_count(), report.verdicts.len(), budget) {
        out.push_str(&line);
        out.push('\n');
    }

    if let Some(audit) = audit {
        out.push('\n');
        out.push_str(&audit_block(audit));
    }
    out
}

// --- JSON ------------------------------------------------------------------

#[derive(Serialize)]
struct JsonKey {
    #[serde(skip_serializing_if = "Option::is_none")]
    msgctxt: Option<String>,
    msgid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    msgid_plural: Option<String>,
}

impl From<&PoKey> for JsonKey {
    fn from(k: &PoKey) -> Self {
        JsonKey {
            msgctxt: k.msgctxt.clone(),
            msgid: k.msgid.clone(),
            msgid_plural: k.msgid_plural.clone(),
        }
    }
}

#[derive(Serialize)]
struct JsonAliveKey {
    #[serde(flatten)]
    key: JsonKey,
    alive_via: &'static str,
}

#[derive(Serialize)]
struct JsonSummary {
    total: usize,
    alive: usize,
    suspect: usize,
    dead: usize,
    blind: usize,
}

#[derive(Serialize)]
struct JsonTracedKey {
    #[serde(flatten)]
    key: JsonKey,
    trace: &'static str,
}

#[derive(Serialize)]
struct JsonAudit {
    dead_total: usize,
    no_trace: usize,
    substring: usize,
    skeleton: usize,
    traced: Vec<JsonTracedKey>,
}

impl From<&AuditReport> for JsonAudit {
    fn from(a: &AuditReport) -> Self {
        JsonAudit {
            dead_total: a.dead_total,
            no_trace: a.no_trace,
            substring: a.substring,
            skeleton: a.skeleton,
            traced: a
                .traced
                .iter()
                .map(|(k, t)| JsonTracedKey {
                    key: JsonKey::from(k),
                    trace: trace_str(*t),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonBudget {
    kind: &'static str,
    limit: f64,
    dead: usize,
    over: bool,
}

/// Build the JSON budget object for a non-default budget (else `None`).
fn json_budget(dead: usize, total: usize, budget: DeadBudget) -> Option<JsonBudget> {
    if !budget.is_set() {
        return None;
    }
    let over = budget.is_exceeded(dead, total);
    Some(match budget {
        DeadBudget::Count(max) => JsonBudget {
            kind: "count",
            limit: max as f64,
            dead,
            over,
        },
        DeadBudget::Ratio(r) => JsonBudget {
            kind: "ratio",
            limit: r,
            dead,
            over,
        },
    })
}

#[derive(Serialize)]
struct JsonReport {
    scope_caveat: &'static str,
    summary: JsonSummary,
    dead: Vec<JsonKey>,
    suspect: Vec<JsonKey>,
    alive: Vec<JsonAliveKey>,
    blind: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget: Option<JsonBudget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audit: Option<JsonAudit>,
}

/// Render the JSON report.
pub fn render_json(
    report: &LivenessReport,
    audit: Option<&AuditReport>,
    budget: DeadBudget,
) -> Result<String> {
    let dead: Vec<JsonKey> = sorted_dead(report).into_iter().map(JsonKey::from).collect();
    let suspect: Vec<JsonKey> = sorted_suspect(report)
        .into_iter()
        .map(JsonKey::from)
        .collect();
    let alive: Vec<JsonAliveKey> = report
        .verdicts
        .iter()
        .filter_map(|v| match v.status {
            Status::Alive(via) => Some(JsonAliveKey {
                key: JsonKey::from(&v.key),
                alive_via: alive_via_str(via),
            }),
            Status::Suspect | Status::Dead => None,
        })
        .collect();

    let json = JsonReport {
        scope_caveat: SCOPE_CAVEAT,
        summary: JsonSummary {
            total: report.verdicts.len(),
            alive: report.alive_count(),
            suspect: report.suspect_count(),
            dead: report.dead_count(),
            blind: report.total_blind(),
        },
        dead,
        suspect,
        alive,
        blind: report.blind.clone(),
        budget: json_budget(report.dead_count(), report.verdicts.len(), budget),
        audit: audit.map(JsonAudit::from),
    };
    Ok(serde_json::to_string_pretty(&json)?)
}

/// Render in the requested format.
pub fn render(
    report: &LivenessReport,
    audit: Option<&AuditReport>,
    budget: DeadBudget,
    format: OutputFormat,
) -> Result<String> {
    match format {
        OutputFormat::Text => Ok(render_text(report, audit, budget)),
        OutputFormat::Json => render_json(report, audit, budget),
    }
}

/// The process exit code implied by the report under `fail_on`, with the Dead
/// gate relaxed to the `budget` (default `Count(0)` ⇒ any dead fails).
/// Operational errors (code `2`) are decided by the caller, not here.
pub fn exit_code(report: &LivenessReport, fail_on: FailOn, budget: DeadBudget) -> i32 {
    let dead_over = budget.is_exceeded(report.dead_count(), report.verdicts.len());
    match fail_on {
        FailOn::Never => 0,
        FailOn::Dead => i32::from(dead_over),
        FailOn::DeadOrBlind => i32::from(dead_over || report.total_blind() > 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::DeadBudget;
    use crate::liveness::{KeyVerdict, LivenessReport, Status};

    fn key(msgid: &str) -> PoKey {
        PoKey {
            msgctxt: None,
            msgid: msgid.to_string(),
            msgid_plural: None,
        }
    }

    fn report() -> LivenessReport {
        let mut blind = BTreeMap::new();
        blind.insert("js".to_string(), 2);
        blind.insert("twig".to_string(), 1);
        LivenessReport {
            verdicts: vec![
                KeyVerdict {
                    key: key("alive_lit"),
                    status: Status::Alive(AliveVia::Literal),
                },
                KeyVerdict {
                    key: key("alive_grd"),
                    status: Status::Alive(AliveVia::Guard),
                },
                KeyVerdict {
                    key: key("dead_two"),
                    status: Status::Dead,
                },
                KeyVerdict {
                    key: key("dead_one"),
                    status: Status::Dead,
                },
            ],
            blind,
        }
    }

    #[test]
    fn text_prints_only_dead_and_scope_header() {
        let text = render_text(&report(), None, DeadBudget::default());
        // header carries the scope caveat
        assert!(text.contains("external consumers"));
        // both dead keys present, sorted
        let one = text.find("dead_one").unwrap();
        let two = text.find("dead_two").unwrap();
        assert!(one < two, "dead keys are sorted");
        // alive keys are NOT listed
        assert!(!text.contains("alive_lit"));
        assert!(!text.contains("alive_grd"));
        // blind summary present
        assert!(text.contains("js=2"));
        assert!(text.contains("twig=1"));
    }

    #[test]
    fn json_is_valid_and_dead_count_matches_text() {
        let r = report();
        let json = render_json(&r, None, DeadBudget::default()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // dead bucket count matches the report
        let dead = parsed["dead"].as_array().unwrap();
        assert_eq!(dead.len(), r.dead_count());
        assert_eq!(dead.len(), sorted_dead(&r).len());

        // alive carries alive_via
        let alive = parsed["alive"].as_array().unwrap();
        assert_eq!(alive.len(), 2);
        let vias: Vec<&str> = alive
            .iter()
            .map(|a| a["alive_via"].as_str().unwrap())
            .collect();
        assert!(vias.contains(&"literal"));
        assert!(vias.contains(&"guard"));

        // blind summary mirrored
        assert_eq!(parsed["blind"]["js"], 2);
        assert_eq!(parsed["summary"]["dead"], 2);
    }

    /// Suspect keys get their own labelled section (text) and bucket (json),
    /// are excluded from `alive`, and do not affect the exit code.
    #[test]
    fn suspect_rendered_and_exit_neutral() {
        let report = LivenessReport {
            verdicts: vec![
                KeyVerdict {
                    key: key("suspect_key"),
                    status: Status::Suspect,
                },
                KeyVerdict {
                    key: key("dead_key"),
                    status: Status::Dead,
                },
            ],
            blind: BTreeMap::new(),
        };

        let text = render_text(&report, None, DeadBudget::default());
        assert!(text.contains("Suspect (1)"));
        assert!(text.contains("suspect_key"));
        assert!(text.contains("1 suspect"));

        let parsed: serde_json::Value =
            serde_json::from_str(&render_json(&report, None, DeadBudget::default()).unwrap())
                .unwrap();
        assert_eq!(parsed["summary"]["suspect"], 1);
        assert_eq!(parsed["suspect"][0]["msgid"], "suspect_key");
        assert!(parsed["alive"].as_array().unwrap().is_empty());

        // Suspect alone (no dead) must not fail the default `dead` gate.
        let suspect_only = LivenessReport {
            verdicts: vec![KeyVerdict {
                key: key("s"),
                status: Status::Suspect,
            }],
            blind: BTreeMap::new(),
        };
        assert_eq!(
            exit_code(&suspect_only, FailOn::Dead, DeadBudget::default()),
            0
        );
    }

    /// With an `AuditReport` attached, the trust block appears in text and the
    /// `audit` object in JSON; the exit code is unaffected.
    #[test]
    fn audit_rendered_and_exit_neutral() {
        let r = report(); // 2 dead
        let audit = AuditReport {
            dead_total: 2,
            no_trace: 1,
            substring: 1,
            skeleton: 0,
            traced: vec![(key("dead_one"), Trace::Substring)],
        };

        let text = render_text(&r, Some(&audit), DeadBudget::default());
        assert!(text.contains("Audit (dead-bucket trust): 2 dead"));
        assert!(text.contains("1 no trace"));
        assert!(text.contains("1 substring"));

        let parsed: serde_json::Value =
            serde_json::from_str(&render_json(&r, Some(&audit), DeadBudget::default()).unwrap())
                .unwrap();
        assert_eq!(parsed["audit"]["dead_total"], 2);
        assert_eq!(parsed["audit"]["no_trace"], 1);
        assert_eq!(parsed["audit"]["substring"], 1);
        assert_eq!(parsed["audit"]["traced"][0]["msgid"], "dead_one");
        assert_eq!(parsed["audit"]["traced"][0]["trace"], "substring");

        // No audit -> no block, no json key.
        assert!(
            !render_text(&r, None, DeadBudget::default()).contains("Audit (dead-bucket trust)")
        );
        let no_audit: serde_json::Value =
            serde_json::from_str(&render_json(&r, None, DeadBudget::default()).unwrap()).unwrap();
        assert!(no_audit.get("audit").is_none());

        // Audit never changes the exit code.
        assert_eq!(exit_code(&r, FailOn::Dead, DeadBudget::default()), 1);
    }

    #[test]
    fn exit_codes_follow_fail_on() {
        let d = DeadBudget::default();
        let r = report(); // 2 dead, 3 blind
        assert_eq!(exit_code(&r, FailOn::Dead, d), 1);
        assert_eq!(exit_code(&r, FailOn::Never, d), 0);
        assert_eq!(exit_code(&r, FailOn::DeadOrBlind, d), 1);

        // a clean report
        let clean = LivenessReport {
            verdicts: vec![KeyVerdict {
                key: key("ok"),
                status: Status::Alive(AliveVia::Literal),
            }],
            blind: BTreeMap::new(),
        };
        assert_eq!(exit_code(&clean, FailOn::Dead, d), 0);
        assert_eq!(exit_code(&clean, FailOn::DeadOrBlind, d), 0);

        // clean of dead but has blind
        let mut blind = BTreeMap::new();
        blind.insert("js".to_string(), 1);
        let blindish = LivenessReport {
            verdicts: vec![],
            blind,
        };
        assert_eq!(exit_code(&blindish, FailOn::Dead, d), 0);
        assert_eq!(exit_code(&blindish, FailOn::DeadOrBlind, d), 1);
    }

    /// An absolute budget relaxes the Dead gate: dead at/under the cap passes,
    /// over the cap fails; `Never` ignores the budget entirely.
    #[test]
    fn budget_count_relaxes_dead_gate() {
        let r = report(); // 2 dead, 3 blind, 4 total
        // at the cap (2) -> within budget; over (1) -> fails.
        assert_eq!(exit_code(&r, FailOn::Dead, DeadBudget::Count(2)), 0);
        assert_eq!(exit_code(&r, FailOn::Dead, DeadBudget::Count(3)), 0);
        assert_eq!(exit_code(&r, FailOn::Dead, DeadBudget::Count(1)), 1);
        // dead-or-blind still fails on blind even when dead is within budget.
        assert_eq!(exit_code(&r, FailOn::DeadOrBlind, DeadBudget::Count(2)), 1);
        // Never never fails, budget or not.
        assert_eq!(exit_code(&r, FailOn::Never, DeadBudget::Count(0)), 0);
    }

    /// A ratio budget fails on the share of the universe; an empty universe never
    /// fails (guarded division).
    #[test]
    fn budget_ratio_uses_universe_share() {
        let r = report(); // 2 dead / 4 total = 50%
        assert_eq!(exit_code(&r, FailOn::Dead, DeadBudget::Ratio(0.5)), 0); // at cap passes
        assert_eq!(exit_code(&r, FailOn::Dead, DeadBudget::Ratio(0.6)), 0);
        assert_eq!(exit_code(&r, FailOn::Dead, DeadBudget::Ratio(0.4)), 1);

        // empty universe -> no division, never over budget.
        let empty = LivenessReport {
            verdicts: vec![],
            blind: BTreeMap::new(),
        };
        assert_eq!(exit_code(&empty, FailOn::Dead, DeadBudget::Ratio(0.0)), 0);
    }

    /// The budget line shows in text and the `budget` object in json only for a
    /// non-default budget; the default `Count(0)` stays silent.
    #[test]
    fn budget_reported_only_when_set() {
        let r = report(); // 2 dead, 4 total

        // within an absolute budget -> headroom line.
        let text = render_text(&r, None, DeadBudget::Count(5));
        assert!(text.contains("Budget: 2 / 5 dead allowed"));
        assert!(text.contains("within budget (3 headroom)"));

        // over the budget -> overage line.
        let over = render_text(&r, None, DeadBudget::Count(1));
        assert!(over.contains("OVER by 1"));

        // default budget -> no budget line at all.
        assert!(!render_text(&r, None, DeadBudget::default()).contains("Budget:"));

        // json carries the budget object only when set.
        let parsed: serde_json::Value =
            serde_json::from_str(&render_json(&r, None, DeadBudget::Count(5)).unwrap()).unwrap();
        assert_eq!(parsed["budget"]["kind"], "count");
        assert_eq!(parsed["budget"]["limit"], 5.0);
        assert_eq!(parsed["budget"]["dead"], 2);
        assert_eq!(parsed["budget"]["over"], false);

        let ratio: serde_json::Value =
            serde_json::from_str(&render_json(&r, None, DeadBudget::Ratio(0.4)).unwrap()).unwrap();
        assert_eq!(ratio["budget"]["kind"], "ratio");
        assert_eq!(ratio["budget"]["over"], true);

        let default: serde_json::Value =
            serde_json::from_str(&render_json(&r, None, DeadBudget::default()).unwrap()).unwrap();
        assert!(default.get("budget").is_none());
    }
}
