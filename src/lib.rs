//! Execra — typed job runtime for external processes.
//!
//! See `SCHEMA.md`, `RUNTIME.md`, and `INTERPRETER.md` for the contract.
//! This crate is the implementation; those documents are the product.

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
