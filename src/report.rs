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
    format!("Blind spots (unverifiable call sites): {}", parts.join(", "))
}

/// Render the text report.
pub fn render_text(report: &LivenessReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} {}\n\n", "dead-poets".bold(), SCOPE_CAVEAT.dimmed()));

    let dead = sorted_dead(report);
    if dead.is_empty() {
        out.push_str(&"No dead keys found.\n".green().to_string());
    } else {
        out.push_str(&format!("{}\n", format!("Dead keys ({}):", dead.len()).bold()));
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
struct JsonReport {
    scope_caveat: &'static str,
    summary: JsonSummary,
    dead: Vec<JsonKey>,
    suspect: Vec<JsonKey>,
    alive: Vec<JsonAliveKey>,
    blind: BTreeMap<String, usize>,
}

/// Render the JSON report.
pub fn render_json(report: &LivenessReport) -> Result<String> {
    let dead: Vec<JsonKey> = sorted_dead(report).into_iter().map(JsonKey::from).collect();
    let suspect: Vec<JsonKey> = sorted_suspect(report).into_iter().map(JsonKey::from).collect();
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
    };
    Ok(serde_json::to_string_pretty(&json)?)
}

/// Render in the requested format.
pub fn render(report: &LivenessReport, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Text => Ok(render_text(report)),
        OutputFormat::Json => render_json(report),
    }
}

/// The process exit code implied by the report under `fail_on`. Operational
/// errors (code `2`) are decided by the caller, not here.
pub fn exit_code(report: &LivenessReport, fail_on: FailOn) -> i32 {
    match fail_on {
        FailOn::Never => 0,
        FailOn::Dead => i32::from(report.dead_count() > 0),
        FailOn::DeadOrBlind => i32::from(report.dead_count() > 0 || report.total_blind() > 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                KeyVerdict { key: key("alive_lit"), status: Status::Alive(AliveVia::Literal) },
                KeyVerdict { key: key("alive_grd"), status: Status::Alive(AliveVia::Guard) },
                KeyVerdict { key: key("dead_two"), status: Status::Dead },
                KeyVerdict { key: key("dead_one"), status: Status::Dead },
            ],
            blind,
        }
    }

    #[test]
    fn text_prints_only_dead_and_scope_header() {
        let text = render_text(&report());
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
        let json = render_json(&r).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // dead bucket count matches the report
        let dead = parsed["dead"].as_array().unwrap();
        assert_eq!(dead.len(), r.dead_count());
        assert_eq!(dead.len(), sorted_dead(&r).len());

        // alive carries alive_via
        let alive = parsed["alive"].as_array().unwrap();
        assert_eq!(alive.len(), 2);
        let vias: Vec<&str> = alive.iter().map(|a| a["alive_via"].as_str().unwrap()).collect();
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
                KeyVerdict { key: key("suspect_key"), status: Status::Suspect },
                KeyVerdict { key: key("dead_key"), status: Status::Dead },
            ],
            blind: BTreeMap::new(),
        };

        let text = render_text(&report);
        assert!(text.contains("Suspect (1)"));
        assert!(text.contains("suspect_key"));
        assert!(text.contains("1 suspect"));

        let parsed: serde_json::Value =
            serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        assert_eq!(parsed["summary"]["suspect"], 1);
        assert_eq!(parsed["suspect"][0]["msgid"], "suspect_key");
        assert!(parsed["alive"].as_array().unwrap().is_empty());

        // Suspect alone (no dead) must not fail the default `dead` gate.
        let suspect_only = LivenessReport {
            verdicts: vec![KeyVerdict { key: key("s"), status: Status::Suspect }],
            blind: BTreeMap::new(),
        };
        assert_eq!(exit_code(&suspect_only, FailOn::Dead), 0);
    }

    #[test]
    fn exit_codes_follow_fail_on() {
        let r = report(); // 2 dead, 3 blind
        assert_eq!(exit_code(&r, FailOn::Dead), 1);
        assert_eq!(exit_code(&r, FailOn::Never), 0);
        assert_eq!(exit_code(&r, FailOn::DeadOrBlind), 1);

        // a clean report
        let clean = LivenessReport {
            verdicts: vec![KeyVerdict {
                key: key("ok"),
                status: Status::Alive(AliveVia::Literal),
            }],
            blind: BTreeMap::new(),
        };
        assert_eq!(exit_code(&clean, FailOn::Dead), 0);
        assert_eq!(exit_code(&clean, FailOn::DeadOrBlind), 0);

        // clean of dead but has blind
        let mut blind = BTreeMap::new();
        blind.insert("js".to_string(), 1);
        let blindish = LivenessReport { verdicts: vec![], blind };
        assert_eq!(exit_code(&blindish, FailOn::Dead), 0);
        assert_eq!(exit_code(&blindish, FailOn::DeadOrBlind), 1);
    }
}
