use clap::Parser;
use dead_poets::cli::{Cli, Commands};

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan { path, config, format, verbose } => {
            // Pipeline wiring lands in later phases; for now confirm argument parsing.
            let _ = (path, config, format, verbose);
            eprintln!("dead-poets: scan pipeline not yet wired (scaffolding phase).");
        }
    }
}
