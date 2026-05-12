//! # Execra
//!
//! Execra is a typed job runtime for external processes. You hand it a
//! [`Command`], it gives you back a [`JobHandle`] that you can either `.await`
//! for the final [`Outcome`] or subscribe to as an event stream. Each job
//! gets phases, progress, findings, and process-group cancellation, behind
//! a single [`Runtime::spawn`] entry point.
//!
//! Interpretation of a CLI's output into structured events is decoupled from
//! the runtime via the [`Interpreter`] trait; see the `INTERPRETER.md` file
//! in the repository for the contract.
//!
//! # Quick example
//!
//! ```no_run
//! use execra::{Command, Outcome, Runtime};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let rt = Runtime::new();
//! let outcome = rt
//!     .spawn(Command::new("echo").arg("hello").label("greet"))?
//!     .await;
//! assert!(matches!(outcome, Outcome::Succeeded { .. }));
//! # Ok(()) }
//! ```
//!
//! Pure in-memory by default: no SQLite, no log files. Opt in to persistence
//! with the builder:
//!
//! ```no_run
//! use execra::Runtime;
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let rt = Runtime::builder()
//!     .history("./jobs.sqlite")
//!     .max_concurrent(4)
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! # Feature flags
//!
//! * `bundled-sqlite` *(default)* — Bundle SQLite with `rusqlite`. Disable to
//!   link against the system `libsqlite3` instead.
//! * `gzip` *(default)* — Enable [`RawOutputPolicy::PersistGzipOnFinalize`]
//!   and pull in `flate2`. Without this feature raw logs can still be
//!   persisted uncompressed via [`RawOutputPolicy::Persist`].
//! * `tauri` — Enable [`execra::tauri`](crate::tauri), the built-in Tauri
//!   plugin and `AppHandle::execra()` extension trait.
//!
//! See `RUNTIME.md` and `INTERPRETER.md` in the repository for the design
//! contract.

pub mod command;
pub mod event;
pub mod finding;
pub mod interpreter;
pub mod job;
pub mod outcome;
pub mod phase;
mod proc_group;
pub mod progress;
pub mod runtime;
pub mod store;
#[cfg(feature = "tauri")]
pub mod tauri;

pub use command::{Command, StdinMode};
pub use event::{Event, Stream};
pub use finding::{Action, Finding, RelatedEntity, Severity};
pub use interpreter::{Context, Interpreter, InterpreterEvent, Line};
pub use job::{Job, JobId, JobState};
pub use outcome::{ExitCode, FailureReason, Outcome};
pub use phase::{Phase, PhaseId};
pub use progress::{Progress, ProgressMetric};
pub use runtime::{
    JobHandle, JobsQuery, RawOutputPolicy, RetentionPolicy, Runtime, RuntimeBuilder,
};
