//! dead-poets — find unused (dead) gettext PO keys across a polyglot codebase.
//!
//! Reusable library behind a standalone CLI. Pipeline of small modules:
//! `config` → `po` (key universe) → `decode` (literal vs guard) →
//! `extract` (per-language adapters) → `guard` → `liveness` → `report`.
//!
//! Everything project-specific lives in `dead-poets.toml`; the engine knows
//! nothing about any particular repository.

pub mod cli;
pub mod config;
pub mod decode;
pub mod extract;
pub mod guard;
pub mod liveness;
pub mod po;
pub mod report;
