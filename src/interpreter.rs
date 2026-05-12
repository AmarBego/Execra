//! The Interpreter contract. See INTERPRETER.md.

use std::time::{Duration, SystemTime};

use crate::command::CommandSpec;
use crate::event::Stream;
use crate::finding::Finding;
use crate::job::JobId;
use crate::outcome::ExitCode;
use crate::phase::Phase;
use crate::progress::Progress;

#[derive(Debug, Clone)]
pub struct Line {
    pub stream: Stream,
    /// Trailing newline already stripped.
    pub text: String,
    pub at: SystemTime,
}

pub struct Context<'a> {
    pub job: JobId,
    pub command: &'a CommandSpec,
    pub current_phase: Option<&'a Phase>,
    pub phase_stack: &'a [Phase],
    pub elapsed: Duration,
}

/// Everything an interpreter is allowed to emit. The runtime translates
/// these into `Event`s, attributing job id, phase id, and timestamps.
#[derive(Debug, Clone)]
pub enum InterpreterEvent {
    EnterPhase {
        name: String,
        label: Option<String>,
    },
    UpdatePhase {
        label: String,
    },
    ExitPhase,

    Progress {
        progress: Progress,
    },
    Label {
        text: String,
    },

    Warning {
        code: Option<String>,
        message: String,
    },
    KnownError {
        code: String,
        message: String,
    },
    Finding {
        finding: Finding,
    },
    Prompt {
        prompt: String,
    },

    Summary {
        text: String,
    },
}

pub trait Interpreter: Send {
    fn on_line(&mut self, ctx: &Context, line: &Line) -> Vec<InterpreterEvent>;

    /// Called exactly once after the process exits and all stdio has drained,
    /// before `Finalized` is emitted. Last chance to flush buffered state or
    /// classify a known error.
    fn on_exit(&mut self, ctx: &Context, exit: &ExitCode) -> Vec<InterpreterEvent>;
}
