use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable within a job. Not global.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhaseId(pub Uuid);

impl PhaseId {
    pub fn new() -> Self {
        PhaseId(Uuid::now_v7())
    }
}

impl Default for PhaseId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub id: PhaseId,
    pub name: String,
    pub label: Option<String>,
    pub entered_at: SystemTime,
}
