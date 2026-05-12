use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::command::CommandSpec;
use crate::finding::Finding;
use crate::job::JobId;
use crate::outcome::{ExitCode, Outcome};
use crate::phase::PhaseId;
use crate::progress::Progress;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// The wire protocol. Every consumer reads this.
///
/// Append-only. Totally ordered per job by `at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    JobCreated {
        job: JobId,
        command: CommandSpec,
        at: SystemTime,
    },
    JobStarted {
        job: JobId,
        pid: u32,
        at: SystemTime,
    },

    PhaseEntered {
        job: JobId,
        phase: PhaseId,
        name: String,
        label: Option<String>,
        at: SystemTime,
    },
    PhaseUpdated {
        job: JobId,
        phase: PhaseId,
        label: String,
        at: SystemTime,
    },
    PhaseExited {
        job: JobId,
        phase: PhaseId,
        at: SystemTime,
    },

    ProgressUpdated {
        job: JobId,
        progress: Progress,
        at: SystemTime,
    },
    LabelUpdated {
        job: JobId,
        label: String,
        at: SystemTime,
    },

    OutputAppended {
        job: JobId,
        stream: Stream,
        line: String,
        at: SystemTime,
    },

    WarningDetected {
        job: JobId,
        code: Option<String>,
        message: String,
        at: SystemTime,
    },
    KnownErrorDetected {
        job: JobId,
        code: String,
        message: String,
        at: SystemTime,
    },
    FindingEmitted {
        job: JobId,
        finding: Finding,
        at: SystemTime,
    },
    PromptDetected {
        job: JobId,
        prompt: String,
        at: SystemTime,
    },

    InterpreterError {
        job: JobId,
        interpreter: String,
        error: String,
        line: Option<String>,
        at: SystemTime,
    },

    Exited {
        job: JobId,
        code: ExitCode,
        at: SystemTime,
    },
    Finalized {
        job: JobId,
        outcome: Outcome,
        at: SystemTime,
    },

    Cancelled {
        job: JobId,
        at: SystemTime,
    },
}

impl Event {
    pub fn job_id(&self) -> JobId {
        match self {
            Event::JobCreated { job, .. }
            | Event::JobStarted { job, .. }
            | Event::PhaseEntered { job, .. }
            | Event::PhaseUpdated { job, .. }
            | Event::PhaseExited { job, .. }
            | Event::ProgressUpdated { job, .. }
            | Event::LabelUpdated { job, .. }
            | Event::OutputAppended { job, .. }
            | Event::WarningDetected { job, .. }
            | Event::KnownErrorDetected { job, .. }
            | Event::FindingEmitted { job, .. }
            | Event::PromptDetected { job, .. }
            | Event::InterpreterError { job, .. }
            | Event::Exited { job, .. }
            | Event::Finalized { job, .. }
            | Event::Cancelled { job, .. } => *job,
        }
    }

    pub fn at(&self) -> SystemTime {
        match self {
            Event::JobCreated { at, .. }
            | Event::JobStarted { at, .. }
            | Event::PhaseEntered { at, .. }
            | Event::PhaseUpdated { at, .. }
            | Event::PhaseExited { at, .. }
            | Event::ProgressUpdated { at, .. }
            | Event::LabelUpdated { at, .. }
            | Event::OutputAppended { at, .. }
            | Event::WarningDetected { at, .. }
            | Event::KnownErrorDetected { at, .. }
            | Event::FindingEmitted { at, .. }
            | Event::PromptDetected { at, .. }
            | Event::InterpreterError { at, .. }
            | Event::Exited { at, .. }
            | Event::Finalized { at, .. }
            | Event::Cancelled { at, .. } => *at,
        }
    }
}
