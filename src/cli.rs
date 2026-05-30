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
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scan_args() {
        let cli = Cli::try_parse_from(["dead-poets", "scan", "./proj", "--format", "json", "-vv"])
            .unwrap();
        let Commands::Scan { path, config, format, verbose, audit } = cli.command;
        assert_eq!(path, "./proj");
        assert_eq!(config, "dead-poets.toml");
        assert_eq!(format, "json");
        assert_eq!(verbose, 2);
        assert!(!audit, "audit defaults to false");
    }

    /// `--audit` flips the flag on.
    #[test]
    fn parses_audit_flag() {
        let cli = Cli::try_parse_from(["dead-poets", "scan", "--audit"]).unwrap();
        let Commands::Scan { audit, .. } = cli.command;
        assert!(audit);
    }

    /// Defaults match the documented PLAN values.
    #[test]
    fn defaults_match_plan() {
        let cli = Cli::try_parse_from(["dead-poets", "scan"]).unwrap();
        let Commands::Scan { path, config, format, verbose, audit } = cli.command;
        assert_eq!(path, ".");
        assert_eq!(config, "dead-poets.toml");
        assert_eq!(format, "text");
        assert_eq!(verbose, 0);
        assert!(!audit);
    }
}
