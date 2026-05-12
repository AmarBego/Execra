use std::time::SystemTime;

use crate::job::Job;
use crate::job::JobState;
use crate::store::JobsFilter;

use super::{Error, Execra};

#[derive(Debug, Clone, Default)]
pub struct JobsQuery {
    pub tag: Option<String>,
    pub state: Option<JobState>,
    pub created_after: Option<SystemTime>,
    pub created_before: Option<SystemTime>,
    pub limit: Option<usize>,
}

impl JobsQuery {
    pub fn with_tag(mut self, t: impl Into<String>) -> Self {
        self.tag = Some(t.into());
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn with_state(mut self, state: JobState) -> Self {
        self.state = Some(state);
        self
    }

    pub fn created_after(mut self, at: SystemTime) -> Self {
        self.created_after = Some(at);
        self
    }

    pub fn created_before(mut self, at: SystemTime) -> Self {
        self.created_before = Some(at);
        self
    }

    pub async fn run(&self, rt: &Execra) -> Result<Vec<Job>, Error> {
        let filter = JobsFilter {
            tag: self.tag.clone(),
            state: self.state,
            created_after: self.created_after,
            created_before: self.created_before,
        };
        rt.inner
            .store
            .list_jobs(self.limit.unwrap_or(100), &filter)
            .await
            .map_err(Error::from)
    }
}
