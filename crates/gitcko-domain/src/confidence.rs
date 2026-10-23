// SPDX-License-Identifier: AGPL-3.0-or-later

/// A finding's final confidence: a finite number from 0 to 1 (REVIEW_ENGINE §15).
/// Constructing one is the only check, so code holding a `Confidence` never sees NaN
/// or an out-of-range value from a model.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f64);

/// The display band. Publication rules depend on it, and it always comes from the
/// unrounded value, never from rounded display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    Low,
    Medium,
    High,
}

/// Initial defaults for uncalibrated routes (REVIEW_ENGINE §15). Calibration per model
/// route, prompt and policy version may replace them later.
const HIGH: f64 = 0.85;
const MEDIUM: f64 = 0.70;

/// A confidence outside 0..=1 or not finite. Refused at the boundary so a bad model
/// value can't sort or band findings wrongly later.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("confidence must be a finite number from 0 to 1, got {0}")]
pub struct ConfidenceError(pub f64);

impl TryFrom<f64> for Confidence {
    type Error = ConfidenceError;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        // Written so NaN fails too: every comparison with NaN is false.
        if (0.0..=1.0).contains(&value) {
            Ok(Confidence(value))
        } else {
            Err(ConfidenceError(value))
        }
    }
}

impl Confidence {
    /// The exact number, unrounded: bands and thresholds compare against this.
    pub fn value(self) -> f64 {
        self.0
    }

    /// High from 0.85, Medium from 0.70, else Low (REVIEW_ENGINE §15). Derived on demand,
    /// never stored, so the number and its band can't disagree.
    pub fn band(self) -> Band {
        if self.0 >= HIGH {
            Band::High
        } else if self.0 >= MEDIUM {
            Band::Medium
        } else {
            Band::Low
        }
    }
}
