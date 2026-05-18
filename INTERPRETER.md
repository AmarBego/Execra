# Interpreter Contract

An interpreter turns a process's output stream into Execra's typed events. The runtime hosts interpreters; it does not embed their logic.

This document defines the contract every interpreter must obey.

---

## The trait

```rust
pub trait Interpreter: Send {
    /// Called for each output line, in order.
    fn on_line(&mut self, ctx: &Context, line: &Line) -> Vec<InterpreterEvent>;

    /// Called once after the process has exited, before Finalized is emitted.
    /// Last chance to attach a summary, flush buffered state, or classify a known error.
    fn on_exit(&mut self, ctx: &Context, exit: &ExitCode) -> Vec<InterpreterEvent>;
}

pub struct Context<'a> {
    pub job: JobId,
    pub command: &'a Command,
    pub current_phase: Option<&'a Phase>,
    pub phase_stack: &'a [Phase],
    pub elapsed: Duration,
}

pub struct Line {
    pub stream: Stream,        // Stdout | Stderr
    pub text: String,          // already stripped of trailing newline
    pub at: SystemTime,
}
```

### What an interpreter may emit

```rust
pub enum InterpreterEvent {
    EnterPhase  { name: String, label: Option<String> },
    UpdatePhase { label: String },
    ExitPhase,

    Progress { progress: Progress },
    Label    { text: String },                          // updates job label

    Warning    { code: Option<String>, message: String },
    KnownError { code: String, message: String },
    Finding    { finding: Finding },                    // persists into Outcome
    Prompt     { prompt: String },

    Summary { text: String },                           // attached at finalization
}
```

### What an interpreter may *not* do

- Cause `JobState` transitions directly.
- Emit `Exited`, `Finalized`, `JobStarted`, etc. Those are runtime-owned.
- Pop a phase it did not push. The runtime drops mismatched pops and emits `InterpreterError`.
- Mutate prior events.
- Block. `on_line` runs on the hot path; if you need expensive work, queue it.

---

## Execution model

- Exactly one interpreter is bound to a job at start. Composition happens by writing a wrapper interpreter, not by stacking.
- `on_line` is called serially, in stream order, for the lifetime of the process.
- A panic or returned error in `on_line` is caught, surfaced as `InterpreterError`, and the interpreter is *disabled for the rest of the job*. The process continues; raw output continues to stream.
- `on_exit` is called exactly once, after the process has exited and `Exited` has been emitted, but before `Finalized`.

Rationale for "disable on panic, don't kill the job": the job is the user's work. Interpretation is metadata. Bad metadata must never destroy work in progress.

---

## Writing an interpreter

Implement the trait. Keep state in `self`. Use whatever regex / parsing approach fits the CLI.

The typical interpreter is 30–80 lines and looks like:

1. A struct holding any cross-line state (current phase tracking, multi-line buffers, counters).
2. Static regexes (e.g. `once_cell::sync::Lazy<Regex>`) for the patterns the CLI emits.
3. An `on_line` that dispatches on those regexes and returns a small `Vec<InterpreterEvent>`.
4. An `on_exit` that flushes any pending state and attaches a summary or known-error classification.

See [examples/scoop_install.rs](examples/scoop_install.rs) and [examples/scoop_doctor.rs](examples/scoop_doctor.rs) for worked references covering phases, byte-progress, multi-line collection, findings with typed actions, and exit-code classification.

### Helpers

The `execra` crate exposes terse constructors so interpreters stay readable:

```rust
Finding::info("scoop.notes", text)
Finding::recommendation("vt.api_missing", "API key not set")
    .with_action(Action::command("Install", "scoop", ["install", "7zip"]))

Progress::bytes_mb(done, total)           // -> Determinate(Bytes { .. })
Progress::indeterminate("verifying")
Progress::fraction(0.42)
```

These are conveniences over the public event types, not new concepts.

---

## Multi-line patterns

Cross-line state , Notes blocks, multi-line stack traces, indented continuations , lives in `self`. No special runtime support needed; the common pattern is a buffer plus a boolean:

```rust
if self.in_notes {
    if line.text.is_empty() {
        self.in_notes = false;
        let msg = std::mem::take(&mut self.notes_buf).join("\n");
        return vec![Event::Finding { finding: Finding::info("scoop.notes", msg) }];
    }
    if !line.text.trim_start().starts_with("---") {
        self.notes_buf.push(line.text.clone());
    }
    return vec![];
}
if line.text == "Notes" { self.in_notes = true; return vec![]; }
```

`on_exit` is the natural place to flush a buffer that didn't terminate cleanly (process killed mid-block).

---

## Errors

Interpreters return `Vec<InterpreterEvent>`. Failure is expressed by emitting nothing (or by panicking, which is caught and reported as `InterpreterError`).

If an interpreter wants to surface a parsing problem without giving up entirely, emit a `Warning` event with a `code` like `"interpreter.unexpected_format"` and keep going.

---

## Future: WASM interpreters

The trait is designed so a `wasmtime`-hosted interpreter can be added later without changing the runtime API. The boundary would be JSON in / JSON out per call:

```
input:  { line, stream, current_phase, elapsed }
output: [InterpreterEvent, ...]
```

This unlocks distributing interpreters as data and writing them in any language that compiles to WASM. Not in v1; called out so the v1 design doesn't preclude it.

A declarative rule-file format (TOML/YAML) was prototyped and dropped. Native Rust adapters turned out to be shorter and clearer for every real case we tried; a DSL only earns its keep once non-Rust authorship or hot-reload becomes a concrete need.

That said, most adapters converge on the same shape: a regex table, `$1` substitution, flat sequential phases, a multi-line block or two, and an on-exit error fallback. The optional `interpret` feature factors *that shape* out as a native-Rust helper — the `rules!` macro plus `RuleInterpreter` (see [`src/interpret.rs`](src/interpret.rs)). It is still ordinary Rust source (patterns and templates are string literals in your crate), not a data DSL, and it's opt-in: hand-writing the trait remains fully supported and the two compose.

---

## Versioning

The `Interpreter` trait is part of the public API. Additive changes to `InterpreterEvent` (new variants, new optional fields) are minor; renames or removals are breaking.
