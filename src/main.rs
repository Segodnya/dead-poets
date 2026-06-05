use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Parser;

use dead_poets::cli::{Cli, Commands};
use dead_poets::config::OutputFormat;
use dead_poets::{budget, report, run};

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
    let outcome = run(Path::new(config_path), Path::new(path), run_audit)?;

    // Gate policy is the CLI's: budget (config knobs overridden by CLI flags) and
    // the fail_on exit gate, read off the config the engine loaded.
    let budget = budget::resolve(cli_max_dead, cli_max_dead_ratio, &outcome.config.output)?;

    print!(
        "{}",
        report::render(&outcome.report, outcome.audit.as_ref(), budget, format)?
    );

    Ok(report::exit_code(
        &outcome.report,
        outcome.config.output.fail_on,
        budget,
    ))
}
