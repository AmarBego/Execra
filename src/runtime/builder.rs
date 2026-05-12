use std::path::PathBuf;
use std::time::Duration;

use crate::store::{JobsFilter, Store};

use super::{Error, RawOutputPolicy, RetentionPolicy, Runtime, RuntimeConfig};

/// Fluent builder for [`Runtime`]. Use this when you need persistence,
/// raw-log files, or non-default tuning. For everything else, prefer
/// [`Runtime::new`].
///
/// All knobs are optional. `build()` is synchronous and returns
/// `Result<Runtime, Error>` — the only fallible step is opening SQLite
/// when [`history`](RuntimeBuilder::history) is set.
#[derive(Debug, Default)]
pub struct RuntimeBuilder {
    history: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    raw_output: Option<RawOutputPolicy>,
    retention: Option<RetentionPolicy>,
    max_concurrent: Option<usize>,
    default_grace_period: Option<Duration>,
}

impl RuntimeBuilder {
    /// Persist job and event history to the given SQLite file. The parent
    /// directory is created if missing.
    pub fn history(mut self, path: impl Into<PathBuf>) -> Self {
        self.history = Some(path.into());
        self
    }

    /// Directory for raw stdout/stderr logs. Implies [`RawOutputPolicy::Persist`]
    /// unless [`raw_output`](Self::raw_output) is set explicitly.
    pub fn log_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_dir = Some(path.into());
        self
    }

    pub fn raw_output(mut self, policy: RawOutputPolicy) -> Self {
        self.raw_output = Some(policy);
        self
    }

    pub fn retention(mut self, policy: RetentionPolicy) -> Self {
        self.retention = Some(policy);
        self
    }

    pub fn max_concurrent(mut self, n: usize) -> Self {
        self.max_concurrent = Some(n);
        self
    }

    pub fn default_grace_period(mut self, d: Duration) -> Self {
        self.default_grace_period = Some(d);
        self
    }

    pub fn build(self) -> Result<Runtime, Error> {
        let mut cfg = RuntimeConfig::default();
        if let Some(p) = self.log_dir {
            cfg.log_dir = Some(p);
        }
        if let Some(r) = self.raw_output {
            cfg.raw_output = r;
        } else if cfg.log_dir.is_some() {
            cfg.raw_output = RawOutputPolicy::Persist;
        }
        if let Some(r) = self.retention {
            cfg.retention = r;
        }
        if let Some(n) = self.max_concurrent {
            cfg.max_concurrent = n;
        }
        if let Some(d) = self.default_grace_period {
            cfg.default_grace_period = d;
        }

        let (store, persisted) = match &self.history {
            Some(path) => {
                let s = Store::open(path)?;
                s.resurrect_stranded_jobs()?;
                let jobs = s.list_jobs(10_000, &JobsFilter::default())?;
                (Some(s), jobs)
            }
            None => (None, Vec::new()),
        };

        Ok(Runtime::from_parts(cfg, store, persisted))
    }
}
