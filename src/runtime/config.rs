use std::path::PathBuf;
use std::time::Duration;

/// Internal knobs the driver needs at spawn time. Constructed by
/// [`RuntimeBuilder`](super::RuntimeBuilder); never authored by consumers.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeConfig {
    pub log_dir: Option<PathBuf>,
    pub raw_output: RawOutputPolicy,
    pub retention: RetentionPolicy,
    pub max_concurrent: usize,
    pub default_grace_period: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            log_dir: None,
            raw_output: RawOutputPolicy::Disabled,
            retention: RetentionPolicy::default(),
            max_concurrent: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            default_grace_period: Duration::from_secs(5),
        }
    }
}

/// How raw stdout/stderr is retained for finished jobs. `Disabled` is the
/// default — raw lines stream through the event bus and are dropped after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawOutputPolicy {
    /// Append every line to `<log_dir>/<job_id>.log` as the job runs.
    Persist,
    /// As `Persist`, but gzip the file when the job finalizes.
    #[cfg(feature = "gzip")]
    PersistGzipOnFinalize,
    /// Keep the last 1024 lines in a per-job in-memory ring buffer.
    MemoryOnly,
    /// Discard raw output past the event broadcast.
    Disabled,
}

#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    pub keep_events_for: Duration,
    pub keep_raw_for: Duration,
    pub pinned_tag: String,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy {
            keep_events_for: Duration::from_secs(60 * 60 * 24 * 30),
            keep_raw_for: Duration::from_secs(60 * 60 * 24 * 7),
            pinned_tag: "pinned".into(),
        }
    }
}
