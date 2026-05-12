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

impl Outcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Succeeded { .. })
    }

    /// Human-readable summary. Prefers the interpreter-provided `summary`
    /// line; falls back to the failure reason or a generic cancellation note.
    pub fn message(&self) -> String {
        match self {
            Outcome::Succeeded { summary, .. } => summary
                .clone()
                .unwrap_or_else(|| "completed successfully".into()),
            Outcome::Cancelled { .. } => "operation was cancelled".into(),
            Outcome::Failed {
                reason, summary, ..
            } => summary.clone().unwrap_or_else(|| reason.message()),
        }
    }

    /// `.await?` ergonomic: success folds the summary into `Ok`, failure
    /// folds the message into `Err`.
    pub fn into_result(self) -> Result<String, String> {
        if self.is_success() {
            Ok(self.message())
        } else {
            Err(self.message())
        }
    }
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

impl FailureReason {
    pub fn message(&self) -> String {
        match self {
            FailureReason::KnownError { message, .. } => message.clone(),
            FailureReason::NonZeroExit { code } => format!("process exited with code {code}"),
            FailureReason::Signal { signal } => {
                format!("process was terminated by signal {signal}")
            }
            FailureReason::SpawnFailed { error } => error.clone(),
            FailureReason::Timeout => "operation timed out".into(),
        }
    }
}
