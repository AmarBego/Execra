use serde::{Deserialize, Serialize};

use crate::finding::Finding;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitCode {
    /// `None` means killed by signal.
    pub code: Option<i32>,
    /// Unix only; `None` on Windows.
    pub signal: Option<i32>,
}

impl ExitCode {
    pub fn from_code(code: i32) -> Self {
        ExitCode {
            code: Some(code),
            signal: None,
        }
    }

    pub fn from_signal(signal: i32) -> Self {
        ExitCode {
            code: None,
            signal: Some(signal),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self.code, Some(0))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Succeeded {
        summary: Option<String>,
        findings: Vec<Finding>,
    },
    Failed {
        reason: FailureReason,
        summary: Option<String>,
        findings: Vec<Finding>,
    },
    Cancelled {
        findings: Vec<Finding>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailureReason {
    NonZeroExit { code: i32 },
    Signal { signal: i32 },
    KnownError { code: String, message: String },
    SpawnFailed { error: String },
    Timeout,
}
