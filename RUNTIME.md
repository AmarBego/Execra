# Runtime API

The public surface for spawning, observing, cancelling, and optionally
persisting external process jobs. Pair this with [INTERPRETER.md](INTERPRETER.md)
for output mapping.

## Runtime

`Runtime` is the cloneable job runtime. Construct it once at application
startup and share clones with command handlers.

```rust
use execra::{Command, Runtime};

let rt = Runtime::new();
let outcome = rt.spawn(Command::new("echo").arg("hi"))?.await;
assert!(outcome.is_success());
```

`Runtime::new()` is synchronous, infallible, and in-memory. It creates no
SQLite database, no raw log directory, and no retention worker.

Opt into persistence and tuning with the builder:

```rust
let rt = Runtime::builder()
    .history("./jobs.sqlite")
    .log_dir("./raw")
    .raw_output(execra::RawOutputPolicy::Persist)
    .max_concurrent(4)
    .build()?;
```

Core methods:

```rust
impl Runtime {
    pub fn new() -> Self;
    pub fn builder() -> RuntimeBuilder;

    pub fn spawn(&self, cmd: Command) -> Result<JobHandle, Error>;
    pub fn cancel(&self, id: JobId) -> Result<(), Error>;

    pub fn recent(&self, n: usize) -> Vec<Job>;
    pub fn running(&self) -> Vec<JobId>;
    pub fn job(&self, id: JobId) -> Option<Job>;
    pub fn jobs(&self) -> JobsQuery;

    pub fn subscribe(&self) -> EventStream;
    pub fn subscribe_job(&self, id: JobId) -> EventStream;
}
```

`spawn` is synchronous: it schedules the driver task and returns a `JobHandle`.
It must be called from inside a Tokio runtime.

## Command

`Command::new(program)` takes a program and args. It does not parse shell
strings.

```rust
Command::new("git").args(["status", "--short"]);
Command::new("scoop").args(["install", "git"]);
Command::new("./tool").arg("--json");
```

Shell helpers are explicit:

```rust
Command::shell("echo hello");       // cmd /C on Windows, sh -c elsewhere
Command::cmd("dir");
Command::sh("ls -la");
Command::powershell("Get-ChildItem");
Command::pwsh("Get-ChildItem");
```

Other builder methods cover env, cwd, stdin, label, tags, timeout, hidden
window behavior, and an optional interpreter:

```rust
Command::new("tool")
    .args(["--scan", "target"])
    .env("NO_COLOR", "1")
    .cwd("./work")
    .label("Scanning target")
    .tags(["scan".to_string()])
    .timeout(std::time::Duration::from_secs(60))
    .interpreter(MyInterpreter);
```

Output bytes are decoded with lossy UTF-8 at the runtime boundary. Invalid
byte sequences are replaced with U+FFFD instead of dropping output handling.

## JobHandle

`Runtime::spawn` returns a `JobHandle`.

```rust
let mut handle = rt.spawn(cmd)?;
let id = handle.id();
let mut events = handle.subscribe();

tokio::spawn(async move {
    while let Some(event) = events.next().await {
        // render, log, or forward
        let _ = event;
    }
});

let outcome = handle.await;
```

`JobHandle` implements `Future<Output = Outcome>`. The same spawn call serves
headless and UI callers: await the handle for the verdict, subscribe to the
handle or runtime for live events, or do both.

## Tauri Plugin

With the `tauri` feature enabled, Execra exposes a first-class Tauri plugin.

```rust
tauri::Builder::default()
    .plugin(execra::tauri::init())
    .run(tauri::generate_context!())
    .unwrap();
```

Use `init_with(runtime)` to pass a preconfigured runtime:

```rust
tauri::Builder::default()
    .plugin(execra::tauri::init_with(
        execra::Runtime::builder()
            .history("./jobs.sqlite")
            .max_concurrent(2)
            .build()
            .expect("open runtime"),
    ));
```

`ExecraExt` adds `app.execra()` to Tauri managers:

```rust
use execra::tauri::ExecraExt;

#[tauri::command]
fn run_tool(app: tauri::AppHandle, args: Vec<String>) -> Result<execra::JobId, String> {
    app.execra()
        .task(execra::Command::new("scrcpy").args(args))
        .channel("scrcpy:log")
        .spawn_tracked()
        .map_err(|e| e.to_string())
}
```

`RuntimeRef::task(cmd)` returns a `TaskBuilder`:

```rust
app.execra()
    .task(cmd)
    .label("Installing git")
    .tag("scoop")
    .interpreter(ScoopInterpreter)
    .channel("operation")
    .on_created(|app, job| {
        // record the current backend job id
        let _ = (app, job);
    })
    .on_output(|app, stream, line| {
        // mirror output into app-owned Rust state
        let _ = (app, stream.as_str(), line);
    })
    .on_interpreter_error(|_app, interpreter, error, line| {
        log::warn!("interpreter error in {interpreter}: {error} ({line:?})");
    })
    .on_finalized(|app, outcome| {
        // clear backend state or trigger follow-up work
        let _ = (app, outcome);
    })
    .await;
```

`.channel(name)` emits every serialized `Event` on one frontend channel.
`.observe(callback)` runs Rust-side observers against the same event stream;
`.on_created`, `.on_output`, `.on_interpreter_error`, and `.on_finalized` are
typed conveniences over `observe`.
For `.spawn()` the forwarder runs in the background; when the task builder is
awaited directly, Execra waits for observers/channel forwarding to see
`Finalized` before returning the `Outcome`.

## Event Subscription

`EventStream` is an async stream of `Event`.

```rust
let mut all = rt.subscribe();
let mut one_job = rt.subscribe_job(id);

while let Some(event) = one_job.next().await {
    // ...
}
```

Subscribers receive events from the moment they subscribe forward. Historical
state comes from `recent`, `running`, `job`, or `jobs`. Lagged broadcast events
are skipped; callers that need exact history should use persistence and query
the stored job/events.

## Cancellation

`rt.cancel(id)` or `handle.cancel()` requests termination of one job.

Semantics:

* Cancellation kills the process group, not just the leader process.
* On Unix, Execra uses `setpgid` and sends signals to the process group.
* On Windows, Execra assigns the child to a Win32 Job Object.
* `cancel()` returns immediately; the job finalizes as `Outcome::Cancelled`
  after the OS reports termination and pipes drain.
* Cancelling an already-finalized job is a no-op.

## Termination Order

Pipe EOF and process exit are independent OS events. Execra waits for stdout
EOF, stderr EOF, and process exit before calling `Interpreter::on_exit`.

Order:

1. `JobCreated`
2. `JobStarted`
3. zero or more output/interpreter events
4. `Exited`
5. `Interpreter::on_exit`
6. `Finalized`

This lets interpreters see final output lines before classifying the outcome.

## Persistence

Persistence is opt-in.

```rust
Runtime::new(); // no disk writes
```

Builder knobs:

```rust
Runtime::builder()
    .history("./jobs.sqlite")          // SQLite job/event history
    .log_dir("./raw")                  // raw stdout/stderr files
    .raw_output(RawOutputPolicy::Persist)
    .retention(RetentionPolicy::default())
    .max_concurrent(4)
    .default_grace_period(Duration::from_secs(5))
    .build()?;
```

Raw output policies:

```rust
pub enum RawOutputPolicy {
    Persist,
    #[cfg(feature = "gzip")]
    PersistGzipOnFinalize,
    MemoryOnly,
    Disabled,
}
```

`Disabled` is the default. Live `OutputAppended` events still stream and
interpreters still see lines, but raw output is not retained after broadcast.

When `history(path)` is set, Execra opens SQLite and persists job snapshots and
events. When `log_dir(path)` is set, raw output can be retained separately from
the event database.

## Non-goals

Execra does not parse shell strings, run in-process work, define job DAGs,
perform distributed scheduling, or ship a catalogue of tool-specific
interpreters. Callers chain jobs with `.await` and provide interpreters for the
CLIs they understand.
