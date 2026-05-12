//! Runtime surface and per-job driver.
//!
//! The public runtime API lives here; implementation details are split into
//! focused submodules so process driving, handles, queries, and streams can
//! evolve independently.

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
use crate::store::{JobsFilter, Store, StoreError};

pub use config::{Config, RawOutputPolicy, RetentionPolicy};
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
    pub(super) config: Config,
    pub(super) store: Store,
    pub(super) events_tx: tokio::sync::broadcast::Sender<Event>,
    pub(super) permits: Arc<Semaphore>,
    pub(super) memory_raw: MemoryRawOutput,
    pub(super) jobs: Mutex<HashMap<JobId, JobEntry>>,
}

#[derive(Clone)]
pub struct Execra {
    pub(super) inner: Arc<Inner>,
}

impl Execra {
    pub async fn open(config: Config) -> Result<Self, Error> {
        let store = Store::open(&config.db_path).await?;
        store.resurrect_stranded_jobs().await?;
        let persisted_jobs = store.list_jobs(10_000, &JobsFilter::default()).await?;

        let (events_tx, _initial_rx) = tokio::sync::broadcast::channel(1024);
        driver::spawn_store_writer(store.clone(), events_tx.subscribe());

        let mut jobs = HashMap::new();
        for job in persisted_jobs {
            jobs.insert(
                job.id,
                JobEntry {
                    cancel: CancellationToken::new(),
                    snapshot: Arc::new(Mutex::new(job)),
                },
            );
        }

        Ok(Execra {
            inner: Arc::new(Inner {
                permits: Arc::new(Semaphore::new(config.max_concurrent.max(1))),
                memory_raw: Arc::new(Mutex::new(HashMap::new())),
                config,
                store,
                events_tx,
                jobs: Mutex::new(jobs),
            }),
        })
    }

    pub async fn spawn(&self, cmd: Command) -> Result<JobHandle, Error> {
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
        let initial_job = snapshot.lock().unwrap().clone();
        self.inner.store.upsert_job(&initial_job).await?;

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

    pub async fn job(&self, id: JobId) -> Option<Job> {
        let in_memory = {
            let jobs = self.inner.jobs.lock().unwrap();
            jobs.get(&id).map(|e| e.snapshot.lock().unwrap().clone())
        };
        if in_memory.is_some() {
            return in_memory;
        }
        self.inner.store.load_job(id).await.ok().flatten()
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
