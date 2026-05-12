use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: PathBuf,
    pub log_dir: PathBuf,
    pub raw_output: RawOutputPolicy,
    pub retention: RetentionPolicy,
    pub max_concurrent: usize,
    pub default_grace_period: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            db_path: PathBuf::from("execra.db"),
            log_dir: PathBuf::from("execra-logs"),
            raw_output: RawOutputPolicy::Persist,
            retention: RetentionPolicy::default(),
            max_concurrent: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            default_grace_period: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawOutputPolicy {
    Persist,
    #[cfg(feature = "gzip")]
    PersistGzipOnFinalize,
    MemoryOnly,
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
