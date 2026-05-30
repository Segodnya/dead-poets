use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Parser;

use dead_poets::cli::{Cli, Commands};
use dead_poets::config::{Config, Output, OutputFormat, resolve_whitelist};
use dead_poets::report::DeadBudget;
use dead_poets::{audit, liveness, po, report, scan};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan {
            path,
            config,
            format,
            verbose,
            audit,
            max_dead,
            max_dead_ratio,
        } => {
            init_logging(verbose);
            match run_scan(&path, &config, &format, audit, max_dead, max_dead_ratio) {
                Ok(code) => ExitCode::from(code as u8),
                // Operational failure (bad config, no PO files, walk error) -> 2.
                Err(err) => {
                    eprintln!("error: {err:#}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        2 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };
    let _ = env_logger::Builder::new().filter_level(level).try_init();
}

fn parse_format(s: &str) -> Result<OutputFormat> {
    match s.to_lowercase().as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => Err(anyhow!("unknown --format '{other}' (expected text|json)")),
    }
}

/// Resolve the dead-key budget from CLI flags (which win wholesale) falling back
/// to the config `[output]` knobs. Absolute and ratio forms are mutually
/// exclusive within a source, and a ratio must lie in `[0, 1]`.
fn resolve_dead_budget(
    cli_max_dead: Option<usize>,
    cli_max_dead_ratio: Option<f64>,
    output: &Output,
) -> Result<DeadBudget> {
    // CLI overrides config entirely when either flag is present.
    let (max_dead, max_dead_ratio, src) = if cli_max_dead.is_some() || cli_max_dead_ratio.is_some()
    {
        (
            cli_max_dead,
            cli_max_dead_ratio,
            "--max-dead / --max-dead-ratio",
        )
    } else {
        (
            output.max_dead,
            output.max_dead_ratio,
            "[output] max_dead / max_dead_ratio",
        )
    };

    match (max_dead, max_dead_ratio) {
        (Some(_), Some(_)) => Err(anyhow!(
            "{src}: set only one of an absolute budget or a ratio, not both"
        )),
        (Some(n), None) => Ok(DeadBudget::Count(n)),
        (None, Some(r)) => {
            if !(0.0..=1.0).contains(&r) {
                return Err(anyhow!(
                    "{src}: max_dead_ratio must be between 0.0 and 1.0 (got {r})"
                ));
            }
            Ok(DeadBudget::Ratio(r))
        }
        (None, None) => Ok(DeadBudget::default()),
    }
}

/// Run the full pipeline. Returns the report-driven exit code (0/1); any error
/// is mapped to exit code 2 by the caller.
fn run_scan(
    path: &str,
    config_path: &str,
    format_str: &str,
    run_audit: bool,
    cli_max_dead: Option<usize>,
    cli_max_dead_ratio: Option<f64>,
) -> Result<i32> {
    let format = parse_format(format_str)?;
    let config_path = Path::new(config_path);
    let cfg = Config::load(config_path)?;
    let budget = resolve_dead_budget(cli_max_dead, cli_max_dead_ratio, &cfg.output)?;

    // Whitelist file paths are resolved relative to the config file's directory.
    let config_dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let whitelist = resolve_whitelist(&cfg.whitelist, config_dir)?;

    // Source roots are resolved relative to the scanned project path.
    let project_root = PathBuf::from(path);
    let roots: Vec<PathBuf> = cfg
        .scan
        .source_roots
        .iter()
        .map(|r| project_root.join(r))
        .collect();

    log::info!("scanning {} root(s) for PO catalogs", roots.len());
    let index = po::load_index(&roots, &cfg.scan.po_patterns, &cfg.scan.ignore_dirs)?;
    log::info!("PO universe: {} unique keys", index.len());

    let usage = scan::scan_sources(
        &roots,
        &cfg.scan.source_extensions,
        &cfg.scan.ignore_dirs,
        &cfg.calls,
        cfg.output.min_guard_len,
    )?;
    log::info!(
        "usage: {} literals, {} guards, {} blind sites",
        usage.literals.len(),
        usage.guards.len(),
        usage.blind.values().sum::<usize>(),
    );

    let result = liveness::classify(&index, &usage, &whitelist);

    // Opt-in advisory pass: grep the Dead bucket against raw source for a trust
    // score. Never touches classification or the exit code.
    let audit_report = if run_audit {
        let dead: Vec<&po::PoKey> = result.dead().map(|v| &v.key).collect();
        log::info!("auditing {} dead keys against raw source", dead.len());
        Some(audit::audit(
            &dead,
            &roots,
            &cfg.scan.source_extensions,
            &cfg.scan.ignore_dirs,
            cfg.output.min_guard_len,
        )?)
    } else {
        None
    };

    print!(
        "{}",
        report::render(&result, audit_report.as_ref(), budget, format)?
    );

    Ok(report::exit_code(&result, cfg.output.fail_on, budget))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dead_poets::report::DeadBudget;

    fn output(max_dead: Option<usize>, max_dead_ratio: Option<f64>) -> Output {
        Output {
            max_dead,
            max_dead_ratio,
            ..Output::default()
        }
    }

    /// CLI flags win over config; absent everywhere -> the default budget.
    #[test]
    fn budget_cli_overrides_config() {
        // CLI absolute beats config ratio entirely.
        let b = resolve_dead_budget(Some(100), None, &output(None, Some(0.5))).unwrap();
        assert_eq!(b, DeadBudget::Count(100));

        // No CLI -> config is used.
        let b = resolve_dead_budget(None, None, &output(Some(42), None)).unwrap();
        assert_eq!(b, DeadBudget::Count(42));

        // Nothing anywhere -> default (any dead fails).
        let b = resolve_dead_budget(None, None, &output(None, None)).unwrap();
        assert_eq!(b, DeadBudget::default());
    }

    /// Both forms in one source is an error; a ratio out of `[0,1]` is an error.
    #[test]
    fn budget_validation_rejects_bad_input() {
        assert!(resolve_dead_budget(Some(1), Some(0.1), &output(None, None)).is_err());
        assert!(resolve_dead_budget(None, None, &output(Some(1), Some(0.1))).is_err());
        assert!(resolve_dead_budget(None, Some(1.5), &output(None, None)).is_err());
        assert!(resolve_dead_budget(None, Some(-0.1), &output(None, None)).is_err());
        // Boundaries are valid.
        assert_eq!(
            resolve_dead_budget(None, Some(0.0), &output(None, None)).unwrap(),
            DeadBudget::Ratio(0.0)
        );
        assert_eq!(
            resolve_dead_budget(None, Some(1.0), &output(None, None)).unwrap(),
            DeadBudget::Ratio(1.0)
        );
    }
}
