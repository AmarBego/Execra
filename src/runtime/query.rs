use std::time::SystemTime;

use crate::job::Job;
use crate::job::JobState;
use crate::store::JobsFilter;

use super::{Error, Runtime};

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

    /// Resolve against persistent history if the runtime was built with
    /// [`history`](super::RuntimeBuilder::history); otherwise returns the
    /// in-memory snapshots filtered by this query.
    pub fn run(&self, rt: &Runtime) -> Result<Vec<Job>, Error> {
        let limit = self.limit.unwrap_or(100);
        if let Some(store) = rt.inner.store.as_ref() {
            let filter = JobsFilter {
                tag: self.tag.clone(),
                state: self.state,
                created_after: self.created_after,
                created_before: self.created_before,
            };
            return store.list_jobs(limit, &filter).map_err(Error::from);
        }
        let jobs = rt.inner.jobs.lock().unwrap();
        let mut out: Vec<Job> = jobs
            .values()
            .map(|e| e.snapshot.lock().unwrap().clone())
            .filter(|j| self.matches(j))
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(limit);
        Ok(out)
    }

    fn matches(&self, job: &Job) -> bool {
        if let Some(state) = self.state {
            if job.state != state {
                return false;
            }
        }
        if let Some(after) = self.created_after {
            if job.created_at < after {
                return false;
            }
        }
        if let Some(before) = self.created_before {
            if job.created_at > before {
                return false;
            }
        }
        if let Some(tag) = &self.tag {
            if !job.command.tags.iter().any(|t| t == tag) {
                return false;
            }
        }
        true
    }
}
