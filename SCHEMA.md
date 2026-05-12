# Execra Schema

The contract. Everything downstream is implementation; this file is the product.

If a change to this document would break a UI consumer, an interpreter author, or a language binding, it is a breaking change. Treat it accordingly.

---

## Core types

### Job

A `Job` is a single invocation of a command, plus everything Execra knows about it.

```rust
pub struct Job {
    pub id: JobId,
    pub command: Command,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub state: JobState,
    pub current_phase: Option<PhaseId>,
    pub progress: Progress,
    pub label: Option<String>,        // human-readable, set by interpreter
    pub exit: Option<ExitCode>,       // raw OS exit, immutable once set
    pub outcome: Option<Outcome>,     // interpreted, set at finalization
}
```

`JobId` is opaque (UUID or ULID). `Command` carries program + args + env + cwd.

### JobState

Coarse lifecycle. UI uses this for top-level status; interpreters cannot mutate it directly.

```rust
pub enum JobState {
    Queued,
    Running,
    Exited,      // OS-level: process has returned an exit code
    Finalized,   // Runtime-level: interpretation complete, terminal
    Cancelled,
}
```

`Exited` and `Finalized` are deliberately distinct. See [Terminal state](#terminal-state).

---

## Progress

The single most load-bearing type. Every UI consumer depends on it. Change with extreme care.

```rust
pub enum Progress {
    Unknown,
    Indeterminate { hint: Option<String> },
    Determinate(ProgressMetric),
}

pub enum ProgressMetric {
    Fraction(f32),                            // 0.0..=1.0, no underlying unit
    Count { done: u64, total: u64 },          // files, items, steps
    Bytes { done: u64, total: Option<u64> },  // total optional: streaming
}
```

### Rules

- `Fraction` values are clamped to `0.0..=1.0` at the runtime boundary. Interpreters that emit `1.2` get `1.0`.
- Every `Determinate` variant can compute a fraction. UIs that want a single number always have one.
- `Bytes.total: Option` because servers without `Content-Length` are real.
- `Indeterminate.hint` is for display only ("downloading..."), never for state.
- Progress is per-job, not per-phase. Phases can scope *interpretation* of progress lines, but the job has one current progress value at any time.
- **Interpreters emit raw units. UIs format.** `Bytes` is bytes; `Count` is items. KB/MB/GB/MiB conversions happen at the interpreter boundary (via helpers like `Progress::bytes_mb(...)`) or at the UI; the wire format is always the lowest unit. This keeps interpreters logic-light and lets UIs render "1.2 MB" or "1.2 MiB" or "1 234 567 B" without re-parsing.

---

## Phases

Phases are a stack. `enter_phase` pushes; `exit_phase` pops. The current phase is the top of the stack.

```rust
pub struct Phase {
    pub id: PhaseId,        // stable within a job
    pub name: String,       // interpreter-defined, e.g. "download", "extract"
    pub label: Option<String>,  // human-readable, e.g. "Downloading git 2.43.0"
    pub entered_at: SystemTime,
}
```

### Rules

- Phases are scoped to a single job. PhaseIds are not global.
- An interpreter cannot pop a phase it did not push. Mismatched pops are dropped and logged as `InterpreterError`.
- If a job exits with phases still on the stack, the runtime pops them in order before finalizing.
- Nested phases are supported. Concurrent phases are not (yet) — model them with labels or wait for a real case.

---

## Events

The event stream is the wire protocol. Every consumer — Tauri, CLI tail, future bindings — reads this.

```rust
pub enum Event {
    JobCreated { job: JobId, command: Command, at: SystemTime },
    JobStarted { job: JobId, pid: u32, at: SystemTime },

    PhaseEntered { job: JobId, phase: PhaseId, name: String, label: Option<String>, at: SystemTime },
    PhaseUpdated { job: JobId, phase: PhaseId, label: String, at: SystemTime },
    PhaseExited  { job: JobId, phase: PhaseId, at: SystemTime },

    ProgressUpdated { job: JobId, progress: Progress, at: SystemTime },
    LabelUpdated    { job: JobId, label: String, at: SystemTime },

    OutputAppended { job: JobId, stream: Stream, line: String, at: SystemTime },

    WarningDetected     { job: JobId, code: Option<String>, message: String, at: SystemTime },
    KnownErrorDetected  { job: JobId, code: String, message: String, at: SystemTime },
    FindingEmitted      { job: JobId, finding: Finding, at: SystemTime },
    PromptDetected      { job: JobId, prompt: String, at: SystemTime },

    InterpreterError { job: JobId, interpreter: String, error: String, line: Option<String>, at: SystemTime },

    Exited    { job: JobId, code: ExitCode, at: SystemTime },
    Finalized { job: JobId, outcome: Outcome, at: SystemTime },

    Cancelled { job: JobId, at: SystemTime },
}

pub enum Stream { Stdout, Stderr }
```

### Rules

- Events are append-only. Once emitted, they are never mutated.
- Events are totally ordered per job by `at`. Across jobs, ordering is best-effort.
- `OutputAppended` is the raw line. Interpretation events (`PhaseEntered`, `ProgressUpdated`, etc.) are emitted *in addition*, not instead.
- `OutputAppended.line` is lossy UTF-8 text. Invalid process bytes are replaced with U+FFFD so UI consumers always receive a serializable line.
- `InterpreterError` never kills the job. The process keeps running; only interpretation is degraded.
- `Exited` is emitted exactly once per job that started. `Finalized` is emitted exactly once per job, period.

---

## Findings

A `Finding` is a structured observation that persists into the job's outcome. Findings exist because not every useful job result is success/failure: `scoop doctor`, linters, audits, and dry-runs all produce *findings* as their primary product, regardless of exit code.

```rust
pub struct Finding {
    pub severity: Severity,
    pub code: String,                 // stable identifier, e.g. "scoop.missing_dependency"
    pub message: String,              // human-readable
    pub action: Option<Action>,       // typed remediation; UIs render as button/link/text
    pub related: Option<RelatedEntity>,
    pub at: SystemTime,
}

pub enum Severity { Info, Recommendation, Warning, Error }
// Wire format (JSON, serde) uses lowercase: "info" | "recommendation" | "warning" | "error".

pub enum Action {
    Command {
        label: String,
        program: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
    },
    Link {
        label: String,
        url: String,
    },
    Instruction {
        label: String,
        text: String,
    },
}

pub enum RelatedEntity {
    Package(String),
    File(PathBuf),
    Url(String),
    Other(String),
}
```

### Action kinds

- `Command` — UIs render as a button. Clicking it runs the program with args (typically as a new Execra job).
- `Link` — UIs render as an anchor. Clicking opens the URL in the system browser.
- `Instruction` — UIs render as copyable text. Used when the remediation is human-only ("Enable Developer Mode in Settings").

Untyped string actions are deliberately not supported. If an interpreter doesn't know enough to classify the action, it emits the finding with `action: None` rather than a string the UI can't act on.

### Findings vs warnings

| | Warning | Finding |
|---|---|---|
| Lifecycle | Streamed, transient | Streamed *and* persisted into `Outcome` |
| Purpose | "Something noteworthy happened" | "Here is a result to act on" |
| Example | "Download retried after timeout" | "7-Zip is missing — install it" |
| Survives finalization | No | Yes, in `Outcome.findings` |

If you're not sure which one to emit: would a UI want to show it in a "results" list after the job ends? If yes, it's a finding. If it's only interesting while the job is running, it's a warning.

---

## Terminal state

```rust
pub struct ExitCode {
    pub code: Option<i32>,    // None means killed by signal
    pub signal: Option<i32>,  // Unix; None on Windows
}

pub enum Outcome {
    Succeeded { summary: Option<String>, findings: Vec<Finding> },
    Failed    { reason: FailureReason, summary: Option<String>, findings: Vec<Finding> },
    Cancelled { findings: Vec<Finding> },
}

pub enum FailureReason {
    NonZeroExit { code: i32 },
    Signal      { signal: i32 },
    KnownError  { code: String, message: String },
    SpawnFailed { error: String },
    Timeout,
}
```

### Rules — these are non-negotiable

1. **Exit code owns terminal state.** A process that exits non-zero is `Failed`, regardless of what its output said.
2. **Interpreters enrich, they do not override.** A rule that matches "Successfully installed" cannot turn a non-zero exit into `Succeeded`. It can only provide a `summary`.
3. **A non-zero exit with a matched `KnownErrorDetected` becomes `Failed { reason: KnownError, .. }`** — the interpretation enriches the failure reason but does not change the verdict.
4. **`Cancelled` is its own thing.** A job killed by the user is not `Failed`. The outcome and the UI treatment differ.

### Sequence at termination

```
process exits
  → runtime emits Exited{code}                  (raw truth)
  → interpreter sees Exited, may emit final events (summary, KnownError, etc.)
  → runtime computes Outcome from exit + interpreter evidence
  → runtime emits Finalized{outcome}             (interpreted truth)
  → job state transitions to Finalized
```

`Exited → Finalized` is the only place interpretation is allowed to add evidence after the fact. Everything else streams.

---

## What the runtime owns (and interpreters cannot touch)

- `JobState` transitions
- `ExitCode` (read from the OS)
- The mapping from `ExitCode + evidence → Outcome`
- Event ordering and persistence
- Phase stack integrity (pops must match pushes)
- Progress clamping and normalization

## What interpreters own (and the runtime will not infer)

- Phase boundaries (`enter`, `exit`, `update`)
- Progress values from output
- Labels and human-readable summaries
- Known-error detection and classification
- Prompt detection

---

## Versioning

This schema is `v0`. Until `v1`:

- Additive changes (new event variants, new optional fields) are minor.
- Renames, removals, semantic changes are breaking and bump the schema version.
- Every persisted event carries a `schema_version`. The runtime refuses to load events from a higher version than it knows.

At `v1`, the wire format freezes. Plan accordingly.
