//! Progress is the single most load-bearing type. See SCHEMA.md.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Progress {
    Unknown,
    Indeterminate { hint: Option<String> },
    Determinate(ProgressMetric),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum ProgressMetric {
    Fraction { value: f32 },
    Count { done: u64, total: u64 },
    Bytes { done: u64, total: Option<u64> },
}

impl Progress {
    pub fn indeterminate(hint: impl Into<String>) -> Self {
        Progress::Indeterminate {
            hint: Some(hint.into()),
        }
    }

    pub fn fraction(value: f32) -> Self {
        Progress::Determinate(ProgressMetric::Fraction {
            value: clamp01(value),
        })
    }

    pub fn count(done: u64, total: u64) -> Self {
        Progress::Determinate(ProgressMetric::Count { done, total })
    }

    pub fn bytes(done: u64, total: Option<u64>) -> Self {
        Progress::Determinate(ProgressMetric::Bytes { done, total })
    }

    /// Convenience for CLIs that report `done MB / total MB`. Stores raw bytes.
    pub fn bytes_mb(done_mb: f64, total_mb: f64) -> Self {
        let to_bytes = |mb: f64| (mb * 1_000_000.0).max(0.0) as u64;
        Progress::Determinate(ProgressMetric::Bytes {
            done: to_bytes(done_mb),
            total: Some(to_bytes(total_mb)),
        })
    }

    /// Normalize to a single 0..=1 ratio if one can be computed.
    pub fn as_fraction(&self) -> Option<f32> {
        match self {
            Progress::Unknown | Progress::Indeterminate { .. } => None,
            Progress::Determinate(m) => match m {
                ProgressMetric::Fraction { value } => Some(*value),
                ProgressMetric::Count { done, total } if *total > 0 => {
                    Some(clamp01(*done as f32 / *total as f32))
                }
                ProgressMetric::Bytes {
                    done,
                    total: Some(total),
                } if *total > 0 => Some(clamp01(*done as f32 / *total as f32)),
                _ => None,
            },
        }
    }

    /// Normalize input that may exceed 1.0 (interpreters get clamped at the runtime boundary).
    #[allow(dead_code)]
    pub(crate) fn normalize(self) -> Self {
        match self {
            Progress::Determinate(ProgressMetric::Fraction { value }) => {
                Progress::Determinate(ProgressMetric::Fraction {
                    value: clamp01(value),
                })
            }
            other => other,
        }
    }
}

impl Default for Progress {
    fn default() -> Self {
        Progress::Unknown
    }
}

fn clamp01(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}
