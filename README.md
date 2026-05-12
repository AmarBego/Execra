# Execra

[![Crates.io](https://img.shields.io/crates/v/execra.svg)](https://crates.io/crates/execra)
[![Documentation](https://docs.rs/execra/badge.svg)](https://docs.rs/execra)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE.md)

> **DO NOT DEPEND ON THIS CRATE.**
>
> Execra is currently in heavy testing inside my own [rScoop] project as
> the load-bearing case for whether it works as a general-purpose public
> crate. Depending on it externally right now is not supported: the API,
> the wire format, and the persistence schema will change without notice,
> and the crate may end up being narrowed back into an rScoop-only
> implementation detail rather than a published library.
>
> The repository is public so the design can be reviewed and so rScoop
> can consume it from crates.io while it stabilises. If and when that
> happens, this notice will be removed and a `1.0` will set the stability
> contract. Until then: read, fork, learn from it — do not `cargo add`
> it in production.
>
> [rScoop]: https://github.com/AmarBego/rScoop

Execra is a typed job runtime for external processes in Rust. It spawns a
child process and returns a job handle that can be awaited for its final
outcome or subscribed to as a stream of structured events: phase changes,
progress updates, findings, warnings, known errors, and terminal state.

The runtime owns process lifecycle, output decoding, process-group
cancellation, timeout enforcement, and persistence. An optional
[`Interpreter`] turns each CLI's output lines into typed events, so the
runtime stays shell-agnostic and every consumer — a CLI, a desktop UI, a
language binding — reads the same event shape.

This crate is the core runtime. A `execra-cli` and `execra-tauri` adapter
live in the [workspace](crates) and consume the same library.

## Status

`0.1.x`, **not for external consumption** (see the notice at the top of
this README). The wire format is `v0` and still subject to change. See
[`SCHEMA.md`](SCHEMA.md) for the versioning policy.

## Usage

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
execra = "0.1"
```

Open the runtime once at application startup (typically held in shared
state), spawn jobs against it, and either await the handle or subscribe to
events. The same `spawn` call serves both headless and UI callers.

```rust
use execra::{Command, Config, Execra, Outcome};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = Execra::open(Config::default()).await?;

    // Headless caller: await the outcome.
    let outcome = rt
        .spawn(Command::new("echo").arg("hello").label("greet"))
        .await?
        .await;
    assert!(matches!(outcome, Outcome::Succeeded { .. }));

    Ok(())
}
```

To drive a UI, keep the handle, subscribe, and forward events:

```rust
# use execra::{Command, Execra};
# async fn run(rt: &Execra) -> Result<(), Box<dyn std::error::Error>> {
let handle = rt.spawn(Command::new("pwsh")
    .args(["-NoProfile", "-Command", "scoop install git"])
    .label("Installing git"))
    .await?;

let id = handle.id();
let mut events = handle.subscribe();
tokio::spawn(async move {
    while let Some(ev) = events.recv().await {
        // forward to the UI
        let _ = ev;
    }
});
# Ok(()) }
```

Cancellation is per-job and kills the entire process group, so spawned
children (for example `aria2` invoked by `scoop`) terminate too:

```rust
# use execra::{Execra, JobId};
# fn run(rt: &Execra, id: JobId) -> Result<(), Box<dyn std::error::Error>> {
rt.cancel(id)?;
# Ok(()) }
```

## Feature flags

* `bundled-sqlite` *(default)* — Build SQLite into `rusqlite`. Disable to
  link against the system `libsqlite3`.
* `gzip` *(default)* — Enable `RawOutputPolicy::PersistGzipOnFinalize` and
  pull in `flate2`. Raw logs can still be persisted uncompressed without
  this feature.

Both default features are additive; disabling either does not change the
event wire format or the public API surface aside from the variant noted
above.

## Concepts

The crate is built around a small number of types. Full definitions are in
[`SCHEMA.md`](SCHEMA.md); the summary below is enough to follow the API.

**`Job`** — one invocation of a [`Command`], plus the runtime's view of it
(state, current phase, progress, exit code, outcome).

**`Event`** — the wire protocol. `JobCreated`, `PhaseEntered`,
`ProgressUpdated`, `OutputAppended`, `FindingEmitted`, `Exited`,
`Finalized`, and so on. Events are append-only and totally ordered per
job. Every consumer reads the same enum.

**`Progress`** — `Unknown`, `Indeterminate { hint }`, or
`Determinate(ProgressMetric)` where `ProgressMetric` is a fraction, an
item count, or a byte count. Interpreters emit raw units; UIs format.

**`Finding`** — a structured observation (`Severity`, stable `code`,
message, optional typed `Action`) that survives finalization in
`Outcome.findings`. This is how `scoop doctor`, linters, audits, and
dry-runs surface results regardless of exit code.

**`Outcome`** — `Succeeded { findings }`, `Failed { reason, findings }`, or
`Cancelled { findings }`. Exit code owns the verdict; interpreters enrich
but do not override.

**`Interpreter`** — the trait that turns output lines into the events
above. One interpreter per job. See [`INTERPRETER.md`](INTERPRETER.md)
and the [`examples/`](examples) for worked references.

## Runtime behaviour

* `Command::new` is shell-agnostic. For convenience, `Command::shell`,
  `Command::cmd`, `Command::sh`, `Command::powershell`, and `Command::pwsh`
  wrap the program in the appropriate platform shell.
* Output bytes are decoded with lossy UTF-8 so invalid byte sequences do
  not drop lines.
* Cancellation kills the process group: `setpgid` + `kill(-pgid, ...)` on
  Unix, a Win32 Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` on
  Windows.
* `Exited` and `Finalized` are distinct events. `Exited` carries the raw
  OS exit code; `Finalized` carries the interpreted [`Outcome`]. Both fire
  exactly once for a job that started.
* Two persistence stores back the runtime: SQLite for jobs, findings, and
  interpreted events; flat files (optionally gzipped on finalization) for
  raw output. This keeps the database small even for chatty CLIs.
* `Config` exposes the database path, log directory, retention policy,
  concurrency cap, and grace period for cancellation. Per-job timeouts go
  on the [`Command`].

## Non-goals

Execra is deliberately not these things:

* A shell parser. Pass `program + args`, or use a shell helper explicitly.
* A way to run in-process work. Filesystem operations, pure-Rust
  transformations, and anything else that is not a child process should
  stay in plain Rust.
* A DAG / composition engine. Callers chain jobs with `.await`.
* A replacement for `cargo` / `git` / `npm` understanding. The runtime
  does not pretend to know what a CLI means without an interpreter; it
  makes interpreters cheap to write instead.
* A way to hide the underlying process. Raw output is always available
  via `OutputAppended` events; interpretation is additive, never a
  replacement.

## Repository layout

* [`SCHEMA.md`](SCHEMA.md) — Types, events, terminal state, ownership rules.
* [`RUNTIME.md`](RUNTIME.md) — `Execra`, `Command`, `JobHandle`,
  subscription, cancellation, persistence.
* [`INTERPRETER.md`](INTERPRETER.md) — The `Interpreter` trait and its
  execution model.
* [`examples/scoop_install.rs`](examples/scoop_install.rs) and
  [`examples/scoop_doctor.rs`](examples/scoop_doctor.rs) — Reference
  interpreters covering phases, byte progress, multi-line collection,
  findings with typed actions, and exit-code classification.
* [`crates/execra-cli`](crates/execra-cli) and
  [`crates/execra-tauri`](crates/execra-tauri) — Adapter crates that
  consume this library.

## License

Licensed under the Apache License, Version 2.0
([LICENSE.md](LICENSE.md) or <http://www.apache.org/licenses/LICENSE-2.0>).

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be licensed as above, without any additional terms or
conditions.

[`Command`]: https://docs.rs/execra/latest/execra/struct.Command.html
[`Execra::spawn`]: https://docs.rs/execra/latest/execra/struct.Execra.html#method.spawn
[`Interpreter`]: https://docs.rs/execra/latest/execra/trait.Interpreter.html
[`JobHandle`]: https://docs.rs/execra/latest/execra/struct.JobHandle.html
[`Outcome`]: https://docs.rs/execra/latest/execra/enum.Outcome.html
