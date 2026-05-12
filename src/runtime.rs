//! Runtime surface and per-job driver.
//!
//! The public runtime API lives here; implementation details are split into
//! focused submodules so process driving, handles, queries, and streams can
//! evolve independently.

mod builder;
mod config;
mod driver;
mod handle;
mod query;
mod raw_output;
mod stream;
mod wait;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use thiserror::Error;
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::command::Command;
use crate::event::Event;
use crate::job::{Job, JobId, JobState};
use crate::progress::Progress;
use crate::store::{Store, StoreError};

pub use builder::RuntimeBuilder;
pub use config::{RawOutputPolicy, RetentionPolicy};
pub(crate) use config::RuntimeConfig;
pub use handle::JobHandle;
pub use query::JobsQuery;
pub use stream::EventStream;

#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown job: {0}")]
    UnknownJob(JobId),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

pub(super) struct JobEntry {
    pub(super) cancel: CancellationToken,
    pub(super) snapshot: Arc<Mutex<Job>>,
}

const MEMORY_RAW_CAPACITY: usize = 1024;
pub(super) type MemoryRawOutput = Arc<Mutex<HashMap<JobId, VecDeque<String>>>>;

pub(super) struct Inner {
    pub(super) config: RuntimeConfig,
    pub(super) store: Option<Store>,
    pub(super) events_tx: tokio::sync::broadcast::Sender<Event>,
    pub(super) permits: Arc<Semaphore>,
    pub(super) memory_raw: MemoryRawOutput,
    pub(super) jobs: Mutex<HashMap<JobId, JobEntry>>,
}

/// Typed job runtime. Owns process lifecycle, output decoding, process-group
/// cancellation, and (optionally) persistence.
///
/// Construct with [`Runtime::new`] for a pure in-memory runtime, or
/// [`Runtime::builder`] when you need persistence or non-default knobs.
#[derive(Clone)]
pub struct Runtime {
    pub(super) inner: Arc<Inner>,
}

impl Runtime {
    /// In-memory runtime with sensible defaults. No SQLite, no log files,
    /// no retention. Infallible and synchronous — safe to call from any
    /// context (including `main()` before a tokio runtime exists).
    pub fn new() -> Self {
        Self::from_parts(RuntimeConfig::default(), None, Vec::new())
    }

    /// Returns a builder for opt-in persistence and tuning.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    pub(crate) fn from_parts(
        config: RuntimeConfig,
        store: Option<Store>,
        persisted: Vec<Job>,
    ) -> Self {
        let (events_tx, _initial_rx) = tokio::sync::broadcast::channel(1024);
        let mut jobs = HashMap::new();
        for job in persisted {
            jobs.insert(
                job.id,
                JobEntry {
                    cancel: CancellationToken::new(),
                    snapshot: Arc::new(Mutex::new(job)),
                },
            );
        }
        Runtime {
            inner: Arc::new(Inner {
                permits: Arc::new(Semaphore::new(config.max_concurrent.max(1))),
                memory_raw: Arc::new(Mutex::new(HashMap::new())),
                config,
                store,
                events_tx,
                jobs: Mutex::new(jobs),
            }),
        }
    }

    /// Spawn a new job. Synchronous: the call returns as soon as the driver
    /// task is scheduled. Must be invoked from inside a tokio runtime context
    /// (the driver itself runs via `tokio::spawn`).
    pub fn spawn(&self, cmd: Command) -> Result<JobHandle, Error> {
        let (spec, stdin, interpreter) = cmd.into_parts();
        let job_id = JobId::new();
        let cancel = CancellationToken::new();
        let now = SystemTime::now();

        let job = Job {
            id: job_id,
            command: spec.clone(),
            created_at: now,
            started_at: None,
            state: JobState::Queued,
            current_phase: None,
            progress: Progress::Unknown,
            label: spec.label.clone(),
            exit: None,
            outcome: None,
        };
        let snapshot = Arc::new(Mutex::new(job));
        if let Some(store) = &self.inner.store {
            let initial_job = snapshot.lock().unwrap().clone();
            store.upsert_job(&initial_job)?;
        }

        self.inner.jobs.lock().unwrap().insert(
            job_id,
            JobEntry {
                cancel: cancel.clone(),
                snapshot: snapshot.clone(),
            },
        );

        let handle_rx = self.inner.events_tx.subscribe();
        let _ = self.inner.events_tx.send(Event::JobCreated {
            job: job_id,
            command: spec.clone(),
            at: now,
        });
        if let Some(store) = &self.inner.store {
            let _ = store.insert_event(&Event::JobCreated {
                job: job_id,
                command: spec.clone(),
                at: now,
            });
        }

        let (outcome_tx, outcome_rx) = oneshot::channel();
        tokio::spawn(driver::drive_job(
            job_id,
            spec,
            stdin,
            interpreter,
            cancel,
            self.inner.events_tx.clone(),
            outcome_tx,
            snapshot,
            self.inner.store.clone(),
            self.inner.config.clone(),
            self.inner.permits.clone(),
            self.inner.memory_raw.clone(),
        ));

        Ok(JobHandle {
            id: job_id,
            inner: self.inner.clone(),
            outcome_rx: Some(outcome_rx),
            initial_rx: Some(handle_rx),
        })
    }

    pub fn cancel(&self, id: JobId) -> Result<(), Error> {
        let jobs = self.inner.jobs.lock().unwrap();
        let entry = jobs.get(&id).ok_or(Error::UnknownJob(id))?;
        if entry.snapshot.lock().unwrap().state == JobState::Finalized {
            return Ok(());
        }
        entry.cancel.cancel();
        Ok(())
    }

    /// Most recent first, in-memory snapshots only. Includes finished jobs
    /// still resident in the cache. For full history, use [`Runtime::jobs`]
    /// with persistence enabled.
    pub fn recent(&self, n: usize) -> Vec<Job> {
        let jobs = self.inner.jobs.lock().unwrap();
        let mut out: Vec<Job> = jobs
            .values()
            .map(|e| e.snapshot.lock().unwrap().clone())
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(n);
        out
    }

    /// Job IDs that are currently `Queued` or `Running`.
    pub fn running(&self) -> Vec<JobId> {
        let jobs = self.inner.jobs.lock().unwrap();
        jobs.iter()
            .filter_map(|(id, e)| {
                let st = e.snapshot.lock().unwrap().state;
                matches!(st, JobState::Queued | JobState::Running).then_some(*id)
            })
            .collect()
    }

    pub fn job(&self, id: JobId) -> Option<Job> {
        let in_memory = {
            let jobs = self.inner.jobs.lock().unwrap();
            jobs.get(&id).map(|e| e.snapshot.lock().unwrap().clone())
        };
        if in_memory.is_some() {
            return in_memory;
        }
        self.inner
            .store
            .as_ref()
            .and_then(|s| s.load_job(id).ok().flatten())
    }

    pub fn jobs(&self) -> JobsQuery {
        JobsQuery::default()
    }

    pub fn subscribe(&self) -> EventStream {
        EventStream::new(self.inner.events_tx.subscribe(), None)
    }

    pub fn subscribe_job(&self, id: JobId) -> EventStream {
        EventStream::new(self.inner.events_tx.subscribe(), Some(id))
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}
