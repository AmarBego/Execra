use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::command::CommandSpec;
use crate::outcome::{ExitCode, Outcome};
use crate::phase::PhaseId;
use crate::progress::Progress;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        JobId(Uuid::now_v7())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    /// OS-level: process has returned an exit code.
    Exited,
    /// Runtime-level: interpretation complete, terminal.
    Finalized,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub command: CommandSpec,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub state: JobState,
    pub current_phase: Option<PhaseId>,
    pub progress: Progress,
    pub label: Option<String>,
    pub exit: Option<ExitCode>,
    pub outcome: Option<Outcome>,
}
