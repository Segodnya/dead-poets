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
    },
}
