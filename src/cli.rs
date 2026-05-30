//! Command-line interface (clap derive). Refined in Phase 9.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "dead-poets")]
#[command(about = "Find unused (dead) gettext keys in your project", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Scan the project for unused PO keys
    Scan {
        /// Root directory of the project
        #[arg(default_value = ".")]
        path: String,

        /// Path to config file
        #[arg(short, long, default_value = "dead-poets.toml")]
        config: String,

        /// Output format: text, json
        #[arg(short, long, default_value = "text")]
        format: String,

        /// Verbosity level (-v, -vv, ...)
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,

        /// Audit the Dead bucket against raw source and print a trust score
        /// (advisory; never changes classification or exit code).
        #[arg(long)]
        audit: bool,

        /// Dead-key budget: fail only when more than N keys are dead. Ratchet it
        /// down over time. Overrides `[output] max_dead`; conflicts with
        /// `--max-dead-ratio`.
        #[arg(long, value_name = "N")]
        max_dead: Option<usize>,

        /// Dead-key budget as a share of the PO universe (0.0–1.0): fail when the
        /// dead ratio exceeds R. Overrides `[output] max_dead_ratio`; conflicts
        /// with `--max-dead`.
        #[arg(long, value_name = "R")]
        max_dead_ratio: Option<f64>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scan_args() {
        let cli = Cli::try_parse_from(["dead-poets", "scan", "./proj", "--format", "json", "-vv"])
            .unwrap();
        let Commands::Scan {
            path,
            config,
            format,
            verbose,
            audit,
            max_dead,
            max_dead_ratio,
        } = cli.command;
        assert_eq!(path, "./proj");
        assert_eq!(config, "dead-poets.toml");
        assert_eq!(format, "json");
        assert_eq!(verbose, 2);
        assert!(!audit, "audit defaults to false");
        assert_eq!(max_dead, None, "max_dead defaults to None");
        assert_eq!(max_dead_ratio, None, "max_dead_ratio defaults to None");
    }

    /// `--audit` flips the flag on.
    #[test]
    fn parses_audit_flag() {
        let cli = Cli::try_parse_from(["dead-poets", "scan", "--audit"]).unwrap();
        let Commands::Scan { audit, .. } = cli.command;
        assert!(audit);
    }

    /// The dead-key budget flags parse into their typed values.
    #[test]
    fn parses_budget_flags() {
        let cli = Cli::try_parse_from(["dead-poets", "scan", "--max-dead", "2900"]).unwrap();
        let Commands::Scan { max_dead, .. } = cli.command;
        assert_eq!(max_dead, Some(2900));

        let cli = Cli::try_parse_from(["dead-poets", "scan", "--max-dead-ratio", "0.15"]).unwrap();
        let Commands::Scan { max_dead_ratio, .. } = cli.command;
        assert_eq!(max_dead_ratio, Some(0.15));
    }

    /// Defaults match the documented PLAN values.
    #[test]
    fn defaults_match_plan() {
        let cli = Cli::try_parse_from(["dead-poets", "scan"]).unwrap();
        let Commands::Scan {
            path,
            config,
            format,
            verbose,
            audit,
            max_dead,
            max_dead_ratio,
        } = cli.command;
        assert_eq!(path, ".");
        assert_eq!(config, "dead-poets.toml");
        assert_eq!(format, "text");
        assert_eq!(verbose, 0);
        assert!(!audit);
        assert_eq!(max_dead, None);
        assert_eq!(max_dead_ratio, None);
    }
}
