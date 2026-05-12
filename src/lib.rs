//! # DO NOT DEPEND ON THIS CRATE
//!
//! Execra is currently in heavy testing inside the author's own `rScoop`
//! project as the load-bearing case for whether it works as a
//! general-purpose public crate. External consumers are **not** supported
//! at this time: the API, the wire format, and the persistence schema
//! will change without notice, and the crate may end up being narrowed
//! back into an `rScoop`-only implementation detail rather than a
//! published library. Read it, fork it, learn from it — do not `cargo
//! add` it in production until a `1.0` release sets a stability
//! contract.
//!
//! ---
//!
//! Execra is a typed job runtime for external processes. You hand it a
//! [`Command`], it gives you back a [`JobHandle`] that you can either `.await`
//! for the final [`Outcome`] or subscribe to as an event stream. Each job
//! gets phases, progress, findings, persistence, and process-group
//! cancellation, behind a single [`Execra::spawn`] entry point.
//!
//! Interpretation of a CLI's output into structured events is decoupled from
//! the runtime via the [`Interpreter`] trait; see the `INTERPRETER.md` file
//! in the repository for the contract.
//!
//! # Quick example
//!
//! ```no_run
//! use execra::{Command, Config, Execra, Outcome};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let rt = Execra::open(Config::default()).await?;
//! let outcome = rt
//!     .spawn(Command::new("echo").arg("hello").label("greet"))
//!     .await?
//!     .await;
//! assert!(matches!(outcome, Outcome::Succeeded { .. }));
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
//!
//! See `SCHEMA.md`, `RUNTIME.md`, and `INTERPRETER.md` in the repository for
//! the full design contract.

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

pub use command::{Command, StdinMode};
pub use event::{Event, Stream};
pub use finding::{Action, Finding, RelatedEntity, Severity};
pub use interpreter::{Context, Interpreter, InterpreterEvent, Line};
pub use job::{Job, JobId, JobState};
pub use outcome::{ExitCode, FailureReason, Outcome};
pub use phase::{Phase, PhaseId};
pub use progress::{Progress, ProgressMetric};
pub use runtime::{Config, Execra, JobHandle, JobsQuery, RawOutputPolicy, RetentionPolicy};
