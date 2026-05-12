# Execra

[![Crates.io](https://img.shields.io/crates/v/execra.svg)](https://crates.io/crates/execra)
[![Documentation](https://docs.rs/execra/badge.svg)](https://docs.rs/execra)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE.md)

Execra is a typed job runtime for Rust apps that wrap external CLI tools.
It owns process lifecycle, output decoding, process-group cancellation,
timeouts, optional persistence, and structured event streaming.

The narrow target is deliberate: Rust backends, especially Tauri apps, that
need to build a GUI around a real command-line program without rewriting job
plumbing for every button.

## Install

```toml
[dependencies]
execra = "1"

# For Tauri apps:
execra = { version = "1", features = ["tauri"] }
```

Default `Runtime::new()` is in-memory only. It does not open SQLite, create
raw log directories, or run retention logic. Opt into persistence explicitly
with `Runtime::builder()`.

## Quick Start

```rust
use execra::{Command, Runtime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = Runtime::new();

    let outcome = rt
        .spawn(Command::new("echo").arg("hello").label("greet"))?
        .await;

    outcome.into_result()?;
    Ok(())
}
```

## Tauri

Enable the `tauri` feature and install the plugin:

```rust
fn main() {
    tauri::Builder::default()
        .plugin(execra::tauri::init())
        .invoke_handler(tauri::generate_handler![run_tool, cancel, history])
        .run(tauri::generate_context!())
        .unwrap();
}
```

Use `app.execra()` from commands:

```rust
use execra::tauri::ExecraExt;
use execra::{Command, JobId};
use tauri::AppHandle;

#[tauri::command]
fn run_tool(app: AppHandle, args: Vec<String>) -> Result<JobId, String> {
    app.execra()
        .task(Command::new("scrcpy").args(args))
        .channel("scrcpy:log")
        .spawn_tracked()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn cancel(app: AppHandle, id: JobId) -> Result<(), String> {
    app.execra().cancel(id).map_err(|e| e.to_string())
}

#[tauri::command]
fn history(app: AppHandle) -> Vec<execra::Job> {
    app.execra().recent(20)
}
```

`.channel(name)` forwards each serialized `Event` to the Tauri event bus under
one channel name. Frontends match on the event's `kind` field.

Use typed hooks when the Rust backend also needs to mirror events into its own
state:

```rust
async fn run_with_backend_state(app: AppHandle, cmd: Command) -> Result<(), String> {
    let outcome = app
        .execra()
        .task(cmd)
        .on_created(|app, job| {
            // remember the current backend job id
            let _ = (app, job);
        })
        .on_output(|app, stream, line| {
            // update your app-owned Rust state here
            let _ = (app, line, stream.as_str());
        })
        .on_finalized(|app, outcome| {
            // clear current-job state or trigger follow-up work
            let _ = (app, outcome);
        })
        .await;

    outcome.into_result().map(|_| ())
}
```

For persisted Tauri history, pass your own runtime:

```rust
tauri::Builder::default()
    .plugin(execra::tauri::init_with(
        execra::Runtime::builder()
            .history("./jobs.sqlite")
            .max_concurrent(2)
            .build()
            .expect("open Execra runtime"),
    ));
```

## Core Concepts

**`Runtime`** - the cloneable process runtime. `Runtime::new()` is in-memory;
`Runtime::builder()` opts into history, raw logs, concurrency, retention, and
grace-period tuning.

**`Command`** - a program plus args, env, cwd, stdin policy, label, tags,
timeout, and optional interpreter. `Command::new` is shell-agnostic; explicit
helpers such as `Command::powershell`, `Command::cmd`, and `Command::sh` wrap
shell use when you need it.

**`JobHandle`** - returned by `Runtime::spawn`. It exposes `id()`, `cancel()`,
`subscribe()`, and implements `Future<Output = Outcome>`.

**`Event`** - the wire protocol. Consumers see `JobCreated`, `JobStarted`,
`OutputAppended`, phase/progress/finding events, `Exited`, `Finalized`, and
`Cancelled`.

**`Interpreter`** - optional per-job logic that maps output lines and final
exit code into typed events. Execra does not guess what a CLI means.

**`Outcome`** - final verdict: `Succeeded`, `Failed`, or `Cancelled`.
`Outcome::is_success()`, `Outcome::message()`, and `Outcome::into_result()`
cover the common consumer mapping.

## Persistence

Persistence is off by default:

```rust
let rt = execra::Runtime::new(); // no files are created
```

Opt in:

```rust
let rt = execra::Runtime::builder()
    .history("./jobs.sqlite")
    .log_dir("./raw")
    .raw_output(execra::RawOutputPolicy::Persist)
    .max_concurrent(4)
    .build()?;
```

`history(path)` enables SQLite job/event history. `log_dir(path)` enables raw
stdout/stderr log files and implies `RawOutputPolicy::Persist` unless you set
another raw-output policy. With the default `gzip` feature, you can choose
`RawOutputPolicy::PersistGzipOnFinalize`.

## Feature Flags

* `bundled-sqlite` (default) - build SQLite into `rusqlite`.
* `gzip` (default) - enable `RawOutputPolicy::PersistGzipOnFinalize`.
* `tauri` - enable `execra::tauri`, the built-in Tauri plugin and extension
  trait.

## Non-goals

Execra is not a shell parser, a task DAG, a generic workflow engine, a
distributed runner, or a catalogue of built-in interpreters. Callers decide
what to run and chain jobs with `.await`.

## Docs

* [RUNTIME.md](RUNTIME.md) - runtime, Tauri plugin, cancellation, persistence.
* [INTERPRETER.md](INTERPRETER.md) - interpreter contract.
* [examples/](examples) - reference interpreters and acceptance snippets.

## License

Licensed under the Apache License, Version 2.0
([LICENSE.md](LICENSE.md) or <http://www.apache.org/licenses/LICENSE-2.0>).
