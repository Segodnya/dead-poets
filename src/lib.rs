//! dead-poets — find unused (dead) gettext PO keys across a polyglot codebase.
//!
//! Reusable library behind a standalone CLI. Pipeline of small modules:
//! `config` → `po` (key universe) → `decode` (literal vs guard) →
//! `extract` (per-language adapters) → `guard` → `liveness` → `report`.
//!
//! Everything project-specific lives in `dead-poets.toml`; the engine knows
//! nothing about any particular repository.

pub mod audit;
pub mod config;
pub mod decode;
pub mod extract;
pub mod guard;
pub mod liveness;
pub mod po;
pub mod scan;

// CLI front-end: argument parsing (`clap`) and terminal/JSON rendering
// (`colored` / `serde_json`). Gated behind the `cli` feature so library
// consumers of the classifier do not pull in terminal dependencies; the
// `dead-poets` binary requires it.
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "cli")]
pub mod report;
