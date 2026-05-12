# Execra

**Never write the process plumbing again.**

Execra is a job runtime for external processes. You spawn a command, it gives you back a typed event stream — phases, progress, findings, terminal outcome — plus persistence, per-job cancellation, and a uniform shape every UI can render.

Shell-agnostic at the core, with ergonomic helpers for common platform shells: same runtime for `bash`, `pwsh`, `zsh`, `awk`, raw binaries, anything you can `exec`.

```rust
let handle = rt.spawn(
    Command::new("pwsh")
        .args(["-NoProfile", "-Command", "scoop install git"])
        .label("Installing git")
        .interpreter(ScoopInstall::new())
).await?;

// Headless caller:
let outcome = handle.await;

// UI caller:
let id = handle.id();
let mut events = handle.subscribe();
while let Some(ev) = events.next().await { forward_to_webview(ev); }
```

Same `spawn`. Different consumers. No second function.

---

## What you get without writing it

- Process spawn, stdout/stderr piping, exit-code handling
- Per-job cancellation that kills the whole process group (so spawned children like `aria2` die too)
- Cross-platform shell helpers (`Command::shell`, `Command::powershell`, `Command::pwsh`)
- Lossy output decoding so non-UTF-8 CLI output still reaches the UI
- Typed event stream: `PhaseEntered`, `ProgressUpdated`, `Finding`, `KnownError`, `Exited`, `Finalized`
- Persistence in SQLite — jobs survive app restarts
- Structured terminal outcomes: `Succeeded { findings }` / `Failed { reason, findings }` / `Cancelled`
- A single subscription API every UI consumer renders the same way

## What you write

- **The interpreter** — a small Rust type that turns your CLI's output lines into events. Typically 30–60 lines per command. See [INTERPRETER.md](INTERPRETER.md).
- **Your domain logic** — which packages to install, which files to check, which CLIs to call. Execra stays out of this.

## What you don't write

- A `run_and_stream` helper
- A global "current operation" cancel event
- Separate headless and UI execution paths
- Custom outcome enums per command
- Process-tree management on Windows or Unix

---

## Documents

- [SCHEMA.md](SCHEMA.md) — types, events, terminal state, ownership rules
- [RUNTIME.md](RUNTIME.md) — `Execra`, `Command`, `JobHandle`, subscription, cancellation, persistence
- [INTERPRETER.md](INTERPRETER.md) — the `Interpreter` trait, execution model, output mapping
- [examples/scoop_install.rs](examples/scoop_install.rs) — reference interpreter for `scoop install`: phases, byte progress, multi-line Notes, known errors, summaries
- [examples/scoop_doctor.rs](examples/scoop_doctor.rs) — reference interpreter for `scoop doctor`: findings with typed actions, the "successful but informational" case

These four documents define the product. Code follows once they read clean against real CLI output.

## Shape

```
┌─────────────────────────────────────────────┐
│  Consumers                                  │
│  - Tauri plugin                             │
│  - CLI (`execra tail --json`)               │
│  - Node / Python bindings (later)           │
│  - HTTP daemon (later)                      │
└─────────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────────┐
│  execra core crate                          │
│  - Job lifecycle                            │
│  - Event stream                             │
│  - Interpreter host                         │
│  - SQLite persistence                       │
└─────────────────────────────────────────────┘
```

The core does not know what is consuming it. Tauri is one consumer among several; the CLI with `--json` output is the universal one — anything that can read JSON lines can drive a UI off Execra.

## Build order

1. `execra` — the core crate (runtime + interpreter trait + sqlite store).
2. `execra-cli` — `run`, `ls`, `logs`, `tail --json`.
3. `execra-tauri` — thin adapter crate for forwarding events to the webview. Should stay small; if it grows, the core API is wrong.
4. Bindings (`napi-rs`, `pyo3`) and a daemon — only if asked for.

## Status

Core runtime is implemented with SQLite persistence, flat-file raw logs, process-group cancellation, timeout enforcement, and a workspace split for CLI/Tauri consumers.

## Non-goals

- **Understanding arbitrary CLIs out of the box.** Execra makes interpretation cheap to write; it does not pretend to know what `cargo` or `git` mean without an interpreter.
- **Shell parsing.** No string splitting, no quoting rules. Pass `program + args`, or invoke a shell yourself.
- **In-process work.** Filesystem operations, in-memory transformations, anything that isn't a child process. Use plain Rust.
- **Job composition.** No DAG, no "run B after A." Callers chain with `.await`. Composition can come later if real demand appears.
- **Replacing shell pipelines.** Execra is for long-running, observable, user-facing jobs — not one-shot scripts.
- **Hiding the underlying process.** Raw output is always available via `OutputAppended` events. Interpretation is additive, never a replacement.
