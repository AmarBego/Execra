# Runtime API

The public surface a host application uses to spawn, observe, cancel, and persist jobs. Pair with [SCHEMA.md](SCHEMA.md) for the event/data model and [INTERPRETER.md](INTERPRETER.md) for output mapping.

---

## Shell-agnostic core, ergonomic shell helpers

Execra has no opinion about what runs inside a process. `Command::new` accepts any program path; the runtime treats every job identically.

```rust
rt.spawn(Command::new("bash").args(["-c", "make build"])).await?;
rt.spawn(Command::new("pwsh").args(["-NoProfile", "-Command", "scoop install git"])).await?;
rt.spawn(Command::new("zsh").args(["-c", "./deploy.sh"])).await?;
rt.spawn(Command::new("awk").args(["-f", "report.awk", "data.txt"])).await?;
rt.spawn(Command::new("./my-binary").args(["--flag"])).await?;
```

For GUI apps that need common shell envelopes without reimplementing platform branching, `Command` also provides explicit helpers:

```rust
Command::shell("echo hello");       // cmd /C on Windows, sh -c elsewhere
Command::cmd("dir");
Command::sh("ls -la");
Command::powershell("Get-ChildItem");
Command::pwsh("Get-ChildItem");
```

This is a deliberate boundary: `Command::new` never parses shell strings, while the helper constructors make shell use explicit and portable.

---

## `Execra`

The runtime handle. One per host application (typically constructed at startup, held in app state, shared across callers).

```rust
pub struct Execra { /* … */ }

impl Execra {
    pub async fn open(config: Config) -> Result<Self, Error>;

    pub async fn spawn(&self, cmd: Command) -> Result<JobHandle, Error>;

    pub fn cancel(&self, id: JobId) -> Result<(), Error>;
    pub async fn job(&self, id: JobId) -> Option<Job>;
    pub fn jobs(&self) -> JobsQuery;          // filter / paginate persisted jobs

    pub fn subscribe(&self) -> EventStream;                  // all events
    pub fn subscribe_job(&self, id: JobId) -> EventStream;   // one job
}
```

`Config` carries the sqlite path, log directory, default concurrency limits, and platform-specific knobs (Windows `CREATE_NO_WINDOW`, Unix process-group setup).

---

## `Command`

Thin builder over the OS process abstraction. No shell awareness.

```rust
pub struct Command { /* … */ }

impl Command {
    pub fn new(program: impl Into<OsString>) -> Self;
    pub fn shell(script: impl Into<String>) -> Self;        // cmd /C or sh -c
    pub fn system_shell(script: impl Into<String>) -> Self; // alias for shell
    pub fn cmd(script: impl Into<String>) -> Self;
    pub fn sh(script: impl Into<String>) -> Self;
    pub fn powershell(script: impl Into<String>) -> Self;
    pub fn pwsh(script: impl Into<String>) -> Self;

    pub fn arg(self, a: impl Into<OsString>) -> Self;
    pub fn args<I, S>(self, args: I) -> Self
        where I: IntoIterator<Item = S>, S: Into<OsString>;

    pub fn env(self, key: impl Into<OsString>, val: impl Into<OsString>) -> Self;
    pub fn envs<I, K, V>(self, vars: I) -> Self
        where I: IntoIterator<Item = (K, V)>, K: Into<OsString>, V: Into<OsString>;
    pub fn env_clear(self) -> Self;

    pub fn cwd(self, dir: impl Into<PathBuf>) -> Self;
    pub fn stdin(self, mode: StdinMode) -> Self;

    pub fn label(self, label: impl Into<String>) -> Self;
    pub fn tags(self, tags: impl IntoIterator<Item = String>) -> Self;
    pub fn interpreter(self, i: impl Interpreter + 'static) -> Self;

    pub fn timeout(self, d: Duration) -> Self;
    pub fn hide_window(self, yes: bool) -> Self;   // Windows: CREATE_NO_WINDOW; default true on Windows
}

pub enum StdinMode {
    Null,                         // default
    Inherit,                      // pass-through (rare)
    Piped(Vec<u8>),               // write bytes, then close
}
```

`label` is the human-readable title surfaced in events. `tags` are user-defined strings useful for filtering (`tags: ["scoop", "cleanup"]`).

Output bytes are decoded with lossy UTF-8 at the runtime boundary. Invalid byte sequences are replaced with U+FFFD instead of dropping the line or terminating output handling.

---

## `JobHandle`

The thing `spawn` returns. Implements `Future<Output = Outcome>` so callers can `.await` completion. Carries the `JobId` so callers who want to keep running (UI streaming) can stash it and subscribe.

```rust
pub struct JobHandle { /* … */ }

impl JobHandle {
    pub fn id(&self) -> JobId;
    pub fn cancel(&self) -> Result<(), Error>;
    pub fn subscribe(&self) -> EventStream;
}

impl Future for JobHandle {
    type Output = Outcome;
    /* polls the underlying job to finalization */
}
```

This is the dual-mode pattern that collapses "headless" and "UI" callers into one code path:

```rust
// Headless: just await the outcome.
let outcome = rt.spawn(cmd).await?.await;
match outcome {
    Outcome::Succeeded { .. } => trigger_next_step().await,
    Outcome::Failed { reason, .. } => log::warn!("failed: {reason:?}"),
    Outcome::Cancelled { .. } => {}
}

// UI: stash the id, subscribe to events, render.
let handle = rt.spawn(cmd).await?;
let id = handle.id();
let mut events = handle.subscribe();
tokio::spawn(async move {
    while let Some(ev) = events.next().await { forward_to_webview(ev); }
});
state.current_job = Some(id);
```

Same `spawn`. Two consumption patterns. No second function.

---

## Cancellation

`rt.cancel(id)` or `handle.cancel()` requests termination of a specific job. There is no global cancel.

Semantics:
- Cancellation **kills the process group**, not just the leader PID. On Unix this means `setpgid` at spawn and `kill(-pgid, SIGTERM)` followed by `SIGKILL` after a grace period. On Windows this means each job is assigned to a Win32 Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Practical consequence: when scoop install gets cancelled, the aria2 child dies too.
- Cancellation is async. `cancel()` returns immediately; the job transitions to `Outcome::Cancelled` once the OS confirms termination.
- A cancelled job's `Finalized` event still fires. Subscribers don't need a separate "was it cancelled" check.
- Cancelling a job that's already finalized is a no-op (returns `Ok(())`), not an error.

---

## Termination order

Pipe EOF and process exit are independent OS events. A naive runtime that triggers `on_exit` on `child.wait()` alone can lose the final lines still buffered in the pipe at exit time , including the summary or known-error line that decides how the job is classified.

Execra guarantees this order:

1. The runtime concurrently drives three futures: stdout-to-EOF, stderr-to-EOF, and `child.wait()`.
2. Every line drained from the pipes is dispatched to `on_line` in stream order.
3. Only after **all three** futures resolve does the runtime call `Interpreter::on_exit` with the captured `ExitCode`.
4. The runtime then computes `Outcome` from exit code + interpreter evidence and emits `Finalized`.

Concretely, this means:

- A summary line printed milliseconds before exit is always delivered to the interpreter before `on_exit` runs.
- A process that closes stdout early and continues working still has its later exit code respected; `on_exit` is not called early.
- Cancellation kills the process group, then awaits the same three futures so cancelled jobs still finalize cleanly.

---

## Event subscription

`EventStream` is an async stream of `Event` (see [SCHEMA.md](SCHEMA.md)). Backed by a broadcast channel.

- Subscribers receive events from the moment they subscribe forward. Historical events come from `rt.job(id)` / `rt.jobs()` queries, not from the live stream.
- Multiple subscribers per job are allowed. The Tauri plugin and a CLI tail can subscribe simultaneously.
- A slow subscriber lagging the channel buffer is dropped (with a `Lagged` notification) rather than blocking event production. UIs should refetch state via `rt.job(id)` after a lag.

---

## Persistence

Two stores, one runtime. The split keeps the SQLite database small and queryable while letting raw output stay cheap.

**SQLite , events, jobs, metadata.**
- Job records (command, label, tags, timestamps, state, outcome).
- Interpreted events: `PhaseEntered`, `ProgressUpdated`, `Finding`, `Warning`, `KnownError`, `Summary`, `Exited`, `Finalized`, etc.
- Queryable via `rt.jobs()` (filter by state, tag, time range).

**Flat files , raw output.**
- One file per job: `<log_dir>/<job_id>.log` (or `.log.gz` after finalization, configurable).
- Append-only during the job; closed and optionally gzipped on `Finalized`.
- `OutputAppended` events surfacing to subscribers are sourced from the flat file. The wire shape is identical regardless of storage , consumers don't know the difference.

This split exists because a `cargo build` or `npm install` can emit 10K+ output lines. Inserting that many rows into SQLite per job destroys write throughput and balloons the database. Flat files are cheap to append, cheap to gzip, cheap to delete, and trivial to tail.

### Config

```rust
pub struct Config {
    pub db_path: PathBuf,                       // SQLite location
    pub log_dir: PathBuf,                       // per-job flat-file directory
    pub raw_output: RawOutputPolicy,            // see below
    pub retention: RetentionPolicy,             // separate for events vs raw logs
    pub max_concurrent: usize,                  // default: num_cpus
    pub default_grace_period: Duration,         // SIGTERM → SIGKILL on cancel
}

pub enum RawOutputPolicy {
    Persist,                  // default: flat file per job
    PersistGzipOnFinalize,    // flat during run, gzip on Finalized
    MemoryOnly,               // ring buffer; events still stream live
    Disabled,                 // drop raw lines after dispatching to interpreter
}

pub struct RetentionPolicy {
    pub keep_events_for: Duration,    // SQLite event rows; default 30 days
    pub keep_raw_for: Duration,       // flat log files; default 7 days
    pub pinned_tag: String,           // jobs with this tag are never pruned
}
```

`Command::timeout(d)` enforces a wall-clock deadline from successful process spawn. On timeout the runtime kills the process group, emits the raw `Exited` event when the OS reports termination, and finalizes as `Outcome::Failed { reason: FailureReason::Timeout, .. }`. User cancellation remains distinct and finalizes as `Outcome::Cancelled`.

`Disabled` is useful for noisy CLIs whose output isn't worth keeping (compile spam, large `tar` operations) and for sensitive jobs whose raw output shouldn't hit disk. Even with `Disabled`, the event stream is unaffected , interpreted events still persist; subscribers still see `OutputAppended` live; nothing is written to the flat file.

### Resumability

A job that was `Running` when the process died at shutdown is resurrected as `Failed { reason: SpawnFailed { error: "host process exited" } }` on next startup, *not* re-run. v1 guarantees the *record* survives; resumability of partially-completed work is a v0.2 concern.

---

## What Execra does not do

Pinning these explicitly so the runtime stays small:

- **Shell parsing.** No string splitting, no quoting rules, no environment interpolation. Pass `program + args`, or invoke a shell yourself.
- **In-process work.** Filesystem operations, in-memory transformations, anything that isn't a child process. Use plain Rust; don't pretend it's a job.
- **Job composition / dependencies.** v1 has no DAG, no "run B after A succeeds." Callers chain with `.await`. Composition is a v0.2 concern at earliest.
- **Distributed execution.** Single host, single process. No cross-machine scheduling.
- **Stdin interactivity.** A job can be fed a static byte buffer via `StdinMode::Piped`, but Execra does not model interactive prompts beyond emitting `PromptDetected`. Handling the prompt is the host app's job.
