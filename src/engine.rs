//! Library entry point — config file in, classification out, behind one call.
//!
//! `run` loads the config, resolves the whitelist and source roots, then drives
//! `po` (key universe) → `scan` (source usage) → `liveness` (classification),
//! with an optional `audit` pass over the Dead bucket. It owns every path-
//! resolution convention (so callers never replicate it) and returns data only:
//! rendering, the dead-key budget, output format, and exit codes are CLI concerns
//! and stay in the binary (`main.rs`), keeping this seam free of the `cli`
//! feature's terminal dependencies.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::audit::{self, AuditReport};
use crate::config::{Config, resolve_whitelist};
use crate::liveness::{self, LivenessReport};
use crate::{po, scan};

/// The outcome of a full run: the loaded config the run used, the per-key
/// liveness verdicts, and — when requested — the advisory audit over the Dead
/// bucket. The config is returned so the caller drives its own gate/exit policy
/// (`output.fail_on`, the budget) without re-reading the file. The key universe
/// size (for a ratio budget) is `report.verdicts.len()`.
#[derive(Debug)]
pub struct Outcome {
    pub config: Config,
    pub report: LivenessReport,
    pub audit: Option<AuditReport>,
}

/// Run the full pipeline: load `config_path`, scan `project_root`, classify.
///
/// `scan.source_roots` are resolved relative to `project_root`; the whitelist
/// file is resolved relative to the config file's own directory (so a config may
/// live outside the scanned tree). With `run_audit`, the Dead bucket is greped
/// against raw source for a trust score — advisory, never changing classification.
pub fn run(config_path: &Path, project_root: &Path, run_audit: bool) -> Result<Outcome> {
    let config = Config::load(config_path)?;

    // The whitelist file is resolved relative to the config file's directory.
    let config_dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let whitelist = resolve_whitelist(&config.whitelist, config_dir)?;

    // Source roots are resolved relative to the scanned project path.
    let roots: Vec<PathBuf> = config
        .scan
        .source_roots
        .iter()
        .map(|r| project_root.join(r))
        .collect();

    log::info!("scanning {} root(s) for PO catalogs", roots.len());
    let index = po::load_index(&roots, &config.scan.po_patterns, &config.scan.ignore_dirs)?;
    log::info!("PO universe: {} unique keys", index.len());

    let usage = scan::scan_sources(
        &roots,
        &config.scan.source_extensions,
        &config.scan.ignore_dirs,
        &config.calls,
        config.guard.min_len,
    )?;
    log::info!(
        "usage: {} literals, {} guards, {} blind sites",
        usage.literals.len(),
        usage.guards.len(),
        usage.blind.values().sum::<usize>(),
    );

    let report = liveness::classify(&index, &usage, &whitelist);

    // Opt-in advisory pass: grep the Dead bucket against raw source for a trust
    // score. Never touches classification or the exit code.
    let audit = if run_audit {
        let dead: Vec<&po::PoKey> = report.dead().map(|v| &v.key).collect();
        log::info!("auditing {} dead keys against raw source", dead.len());
        Some(audit::audit(
            &dead,
            &roots,
            &config.scan.source_extensions,
            &config.scan.ignore_dirs,
            config.guard.min_len,
        )?)
    } else {
        None
    };

    Ok(Outcome {
        config,
        report,
        audit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dead-poets-engine-{tag}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const HEADER: &str = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n";

    /// A one-PO + one-PHP fixture: `used` is referenced, `gone` is not — `run`
    /// loads the config, reports one alive and one dead; `run_audit` populates
    /// the audit.
    #[test]
    fn run_classifies_used_and_dead() {
        let dir = temp_dir("classify");
        write(
            &dir,
            "messages.po",
            &format!("{HEADER}msgid \"used\"\nmsgstr \"u\"\n\nmsgid \"gone\"\nmsgstr \"g\"\n"),
        );
        write(&dir, "app.php", "<?php i18n('used'); ?>");
        let cfg_path = write(
            &dir,
            "dp.toml",
            "[scan]\npo_patterns = [\"**/*.po\"]\nsource_extensions = [\"php\"]\n\n\
             [[calls]]\nlang = \"php\"\nkind = \"function\"\nname = \"i18n\"\n",
        );

        let out = run(&cfg_path, &dir, false).unwrap();
        assert_eq!(out.report.alive_count(), 1);
        assert_eq!(out.report.dead_count(), 1);
        assert!(out.report.dead().any(|v| v.key.msgid == "gone"));
        assert!(out.audit.is_none(), "no audit without run_audit");

        let out = run(&cfg_path, &dir, true).unwrap();
        assert!(out.audit.is_some(), "run_audit populates the audit");

        std::fs::remove_dir_all(&dir).ok();
    }
}
