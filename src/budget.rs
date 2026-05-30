//! Dead-key budget (ratchet) — the type, its gate semantics, and resolution.
//!
//! A real catalog starts with thousands of dead keys, so failing on *any* is
//! unusable. The budget relaxes the Dead gate to a debt ceiling that is lowered
//! over time. This module owns the budget end-to-end so a library consumer of the
//! classifier gets the same gate the CLI uses — the reporter only renders it.
//!
//! Absolute (`Count`) and ratio (`Ratio`) forms are mutually exclusive, and the
//! CLI overrides config wholesale. That invariant is enforced once, in
//! [`resolve`] — the single point both CLI flags and config knobs flow through.

use anyhow::{Result, anyhow};

use crate::config::Output;

/// The resolved dead-key budget the exit gate enforces. `Count(0)` (the default)
/// reproduces the historical behaviour — any dead key fails.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeadBudget {
    /// Absolute cap: fail when `dead > Count`.
    Count(usize),
    /// Share of the universe: fail when `dead / total > Ratio`.
    Ratio(f64),
}

impl Default for DeadBudget {
    fn default() -> Self {
        DeadBudget::Count(0)
    }
}

impl DeadBudget {
    /// Whether the Dead bucket exceeds this budget.
    pub fn is_exceeded(self, dead: usize, total: usize) -> bool {
        match self {
            DeadBudget::Count(max) => dead > max,
            DeadBudget::Ratio(r) => total > 0 && (dead as f64 / total as f64) > r,
        }
    }

    /// A non-default budget the user explicitly set — drives whether the budget
    /// line is shown. `Count(0)` is the historical default and stays silent.
    pub fn is_set(self) -> bool {
        !matches!(self, DeadBudget::Count(0))
    }
}

/// Resolve the dead-key budget from CLI flags (which win wholesale) falling back
/// to the config `[output]` knobs. Absolute and ratio forms are mutually
/// exclusive within a source, and a ratio must lie in `[0, 1]`.
pub fn resolve(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn output(max_dead: Option<usize>, max_dead_ratio: Option<f64>) -> Output {
        Output {
            max_dead,
            max_dead_ratio,
            ..Output::default()
        }
    }

    /// CLI flags win over config; absent everywhere -> the default budget.
    #[test]
    fn cli_overrides_config() {
        // CLI absolute beats config ratio entirely.
        let b = resolve(Some(100), None, &output(None, Some(0.5))).unwrap();
        assert_eq!(b, DeadBudget::Count(100));

        // No CLI -> config is used.
        let b = resolve(None, None, &output(Some(42), None)).unwrap();
        assert_eq!(b, DeadBudget::Count(42));

        // Nothing anywhere -> default (any dead fails).
        let b = resolve(None, None, &output(None, None)).unwrap();
        assert_eq!(b, DeadBudget::default());
    }

    /// Both forms in one source is an error; a ratio out of `[0,1]` is an error.
    #[test]
    fn validation_rejects_bad_input() {
        assert!(resolve(Some(1), Some(0.1), &output(None, None)).is_err());
        assert!(resolve(None, None, &output(Some(1), Some(0.1))).is_err());
        assert!(resolve(None, Some(1.5), &output(None, None)).is_err());
        assert!(resolve(None, Some(-0.1), &output(None, None)).is_err());
        // Boundaries are valid.
        assert_eq!(
            resolve(None, Some(0.0), &output(None, None)).unwrap(),
            DeadBudget::Ratio(0.0)
        );
        assert_eq!(
            resolve(None, Some(1.0), &output(None, None)).unwrap(),
            DeadBudget::Ratio(1.0)
        );
    }

    /// Gate semantics: absolute cap and ratio share, with guarded division.
    #[test]
    fn is_exceeded_count_and_ratio() {
        assert!(DeadBudget::Count(2).is_exceeded(3, 10));
        assert!(!DeadBudget::Count(2).is_exceeded(2, 10));
        assert!(DeadBudget::Ratio(0.4).is_exceeded(5, 10));
        assert!(!DeadBudget::Ratio(0.5).is_exceeded(5, 10));
        // Empty universe never exceeds a ratio (no division).
        assert!(!DeadBudget::Ratio(0.0).is_exceeded(0, 0));
    }

    /// Only a non-default budget counts as "set".
    #[test]
    fn is_set_only_for_non_default() {
        assert!(!DeadBudget::default().is_set());
        assert!(!DeadBudget::Count(0).is_set());
        assert!(DeadBudget::Count(1).is_set());
        assert!(DeadBudget::Ratio(0.1).is_set());
    }
}
