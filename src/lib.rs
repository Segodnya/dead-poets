//! dead-poets — find unused (dead) gettext PO keys across a polyglot codebase.
//!
//! Reusable library behind a standalone CLI. Pipeline of small modules:
//! `config` → `po` (key universe) → `decode` (literal vs guard) →
//! `extract` (per-language adapters) → `guard` → `liveness` → `report`.
//!
//! Everything project-specific lives in `dead-poets.toml`; the engine knows
//! nothing about any particular repository.
//!
//! **Scope:** the key universe source is **gettext `.po`/`.pot` only**. The
//! reference side is polyglot (PHP, Twig, JS/TS), but JSON/YAML/`.properties`/
//! `.arb` i18n catalogs (react-i18next, vue-i18n, Rails, Flutter) are out of
//! scope by design.
//!
//! Library consumers call [`run`] for the whole pipeline; see [`engine`].

pub mod audit;
pub mod budget;
pub mod config;
pub mod decode;
pub mod engine;
pub mod extract;
pub mod guard;
pub mod liveness;
pub mod po;
pub mod scan;
pub mod walk;

pub use engine::{Outcome, run};

// CLI front-end: argument parsing (`clap`) and terminal/JSON rendering
// (`colored` / `serde_json`). Gated behind the `cli` feature so library
// consumers of the classifier do not pull in terminal dependencies; the
// `dead-poets` binary requires it.
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "cli")]
pub mod report;
