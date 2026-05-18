//! Tauri integration. Gate-feature: `tauri`.
//!
//! Registers a [`Runtime`] as managed state and exposes an ergonomic
//! `app.execra()` accessor. The intended consumer surface is:
//!
//! ```ignore
//! tauri::Builder::default()
//!     .plugin(execra::tauri::init())
//!     .invoke_handler(tauri::generate_handler![run_tool, cancel, history])
//!     .run(tauri::generate_context!())
//!     .unwrap();
//!
//! #[tauri::command]
//! fn run_tool(app: tauri::AppHandle, args: Vec<String>) -> Result<execra::JobId, String> {
//!     use execra::tauri::ExecraExt;
//!     app.execra()
//!         .task(execra::Command::new("scrcpy").args(args))
//!         .channel("scrcpy:log")
//!         .spawn_tracked()
//!         .map_err(|e| e.to_string())
//! }
//! ```
//!
//! When [`TaskBuilder::channel`] is set, every [`Event`] for that job is
//! re-emitted to the Tauri event bus under the given channel name. The
//! payload is the serialized [`Event`] itself — one schema, one channel,
//! frontend pattern-matches on the `kind` discriminant.
//!
//! [`TaskBuilder::observe`] is the backend-side companion to `channel`: it
//! lets apps update their own Rust state from the same event stream without
//! writing a subscribe loop in every command. Typed helpers such as
//! [`TaskBuilder::on_output`] cover the common cases.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Emitter, Manager, Runtime as TauriRuntime};
use tokio::task::JoinHandle;

use crate::command::Command;
use crate::event::{Event, Stream};
use crate::interpreter::Interpreter;
use crate::job::{Job, JobId};
use crate::outcome::{FailureReason, Outcome};
use crate::progress::Progress;
use crate::runtime::{Error, EventStream, JobHandle, Runtime};

type EventObserver<R> = Box<dyn Fn(&AppHandle<R>, &Event) + Send + 'static>;
type CreepFn = Box<dyn Fn(&str) -> Option<(f32, f32)> + Send + Sync + 'static>;

/// Synthetic-progress ticker for a phase that has no determinate signal of
/// its own. Without it the bar sits frozen while a long opaque step (hashing
/// a multi-GB file, an installer script) runs. It eases the fraction toward
/// the phase's end but stops short of it, so a real boundary/byte signal
/// always has somewhere to take over.
struct CreepState {
    cap: f32,
    cur: f32,
    done: bool,
}

impl CreepState {
    fn new(start: f32, end: f32) -> Self {
        Self {
            // Stop just shy of the end so the next phase boundary still has
            // distance to advance over.
            cap: start + 0.95 * (end - start),
            cur: start,
            done: false,
        }
    }

    /// Advance ~2% of the remaining distance. Returns `Some(fraction)` only
    /// when the rounded whole-percent changed (sub-percent bumps don't move
    /// pixels and aren't worth an event).
    fn tick(&mut self) -> Option<f32> {
        if self.done {
            return None;
        }
        let cur = self.cur;
        if cur + 0.0001 >= self.cap {
            self.done = true;
            return None;
        }
        let new = (cur + (self.cap - cur) * 0.02).min(self.cap);
        let changed = (new * 100.0).round() as i32 != (cur * 100.0).round() as i32;
        self.cur = new;
        changed.then_some(new)
    }
}

/// Plugin with a default in-memory [`Runtime`].
pub fn init<R: TauriRuntime>() -> TauriPlugin<R> {
    init_with(Runtime::new())
}

/// Plugin with a pre-built [`Runtime`]. Use this when you want persistence
/// or custom tuning:
///
/// ```ignore
/// execra::tauri::init_with(
///     execra::Runtime::builder()
///         .history("./jobs.sqlite")
///         .max_concurrent(4)
///         .build()
///         .expect("open runtime"),
/// )
/// ```
pub fn init_with<R: TauriRuntime>(rt: Runtime) -> TauriPlugin<R> {
    // The runtime ships through `setup` because the `Builder::setup` callback
    // is `FnOnce` only via a `Mutex<Option<…>>` shuttle.
    let slot = Mutex::new(Some(rt));
    Builder::new("execra")
        .setup(move |app, _api| {
            let rt = slot
                .lock()
                .unwrap()
                .take()
                .expect("execra plugin setup called twice");
            app.manage(rt);
            Ok(())
        })
        .build()
}

/// Extension trait. Implemented for any `Manager<R>` so `AppHandle`,
/// `Window`, `WebviewWindow`, and `App` all get an `.execra()` accessor.
pub trait ExecraExt<R: TauriRuntime> {
    fn execra(&self) -> RuntimeRef<R>;
}

impl<R: TauriRuntime, M: Manager<R>> ExecraExt<R> for M {
    fn execra(&self) -> RuntimeRef<R> {
        let rt = self.state::<Runtime>().inner().clone();
        RuntimeRef {
            rt,
            app: self.app_handle().clone(),
        }
    }
}

/// Handle to the runtime managed by the plugin. Cheap to clone (just an
/// `Arc` under the hood).
#[derive(Clone)]
pub struct RuntimeRef<R: TauriRuntime> {
    rt: Runtime,
    app: AppHandle<R>,
}

impl<R: TauriRuntime> RuntimeRef<R> {
    /// Start a fluent task builder.
    pub fn task(&self, cmd: Command) -> TaskBuilder<R> {
        TaskBuilder {
            app: self.app.clone(),
            rt: self.rt.clone(),
            cmd,
            channel: None,
            observers: Vec::new(),
            tags: Vec::new(),
            creep: None,
        }
    }

    pub fn cancel(&self, id: JobId) -> Result<(), Error> {
        self.rt.cancel(id)
    }

    /// Most recent jobs (snapshots), newest first.
    pub fn recent(&self, n: usize) -> Vec<Job> {
        self.rt.recent(n)
    }

    /// Currently `Queued` or `Running`.
    pub fn running(&self) -> Vec<JobId> {
        self.rt.running()
    }

    pub fn subscribe(&self) -> EventStream {
        self.rt.subscribe()
    }

    pub fn subscribe_job(&self, id: JobId) -> EventStream {
        self.rt.subscribe_job(id)
    }

    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }
}

/// Fluent builder for a single task. Terminate with one of [`spawn`],
/// [`spawn_tracked`], or `.await` (returns the [`Outcome`]).
///
/// [`spawn`]: TaskBuilder::spawn
/// [`spawn_tracked`]: TaskBuilder::spawn_tracked
pub struct TaskBuilder<R: TauriRuntime> {
    app: AppHandle<R>,
    rt: Runtime,
    cmd: Command,
    channel: Option<String>,
    observers: Vec<EventObserver<R>>,
    tags: Vec<String>,
    creep: Option<CreepFn>,
}

impl<R: TauriRuntime> TaskBuilder<R> {
    /// Forward every event for this job to the Tauri event bus under
    /// `name`. The payload is the typed [`Event`] enum.
    pub fn channel(mut self, name: impl Into<String>) -> Self {
        self.channel = Some(name.into());
        self
    }

    /// Observe every event for this job on the Rust side. Use this to update
    /// app-owned backend state, emit custom events, redact output before
    /// forwarding, or trigger app-specific follow-up work.
    ///
    /// Observers run on a background task for [`spawn`](Self::spawn). When the
    /// task builder itself is `.await`ed, Execra waits for observers to drain
    /// through `Finalized` before returning the [`Outcome`].
    pub fn observe<F>(mut self, f: F) -> Self
    where
        F: Fn(&AppHandle<R>, &Event) + Send + 'static,
    {
        self.observers.push(Box::new(f));
        self
    }

    /// Observe job creation for this task.
    pub fn on_created<F>(self, f: F) -> Self
    where
        F: Fn(&AppHandle<R>, JobId) + Send + 'static,
    {
        self.observe(move |app, event| {
            if let Event::JobCreated { job, .. } = event {
                f(app, *job);
            }
        })
    }

    /// Observe raw output lines for this task.
    pub fn on_output<F>(self, f: F) -> Self
    where
        F: Fn(&AppHandle<R>, Stream, &str) + Send + 'static,
    {
        self.observe(move |app, event| {
            if let Event::OutputAppended { stream, line, .. } = event {
                f(app, *stream, line);
            }
        })
    }

    /// Observe interpreter panics/errors for this task.
    pub fn on_interpreter_error<F>(self, f: F) -> Self
    where
        F: Fn(&AppHandle<R>, &str, &str, Option<&str>) + Send + 'static,
    {
        self.observe(move |app, event| {
            if let Event::InterpreterError {
                interpreter,
                error,
                line,
                ..
            } = event
            {
                f(app, interpreter, error, line.as_deref());
            }
        })
    }

    /// Observe finalization for this task.
    pub fn on_finalized<F>(self, f: F) -> Self
    where
        F: Fn(&AppHandle<R>, &Outcome) + Send + 'static,
    {
        self.observe(move |app, event| {
            if let Event::Finalized { outcome, .. } = event {
                f(app, outcome);
            }
        })
    }

    /// Set the job label (overrides any label already on the [`Command`]).
    pub fn label(mut self, text: impl Into<String>) -> Self {
        self.cmd = self.cmd.label(text);
        self
    }

    /// Tag a job for later filtering via `Runtime::jobs().with_tag(...)`.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Attach an interpreter to translate stdout/stderr lines into typed
    /// [`Event`]s. Omit when the tool's output isn't structured.
    pub fn interpreter<I: Interpreter + Send + 'static>(mut self, i: I) -> Self {
        self.cmd = self.cmd.interpreter(i);
        self
    }

    /// Synthetic progress for opaque phases. `f` maps a phase name to the
    /// fraction slice `(start, end)` it occupies; return `None` for phases
    /// that already emit a real signal (e.g. byte progress) so the ticker
    /// stays out of their way.
    ///
    /// While a phase with a range is open, a background ticker eases the
    /// progress fraction toward `end` (stopping short of it) and emits
    /// synthetic [`Event::ProgressUpdated`] to observers and the channel, so
    /// the bar shows motion instead of freezing. The ticker is cancelled when
    /// the phase exits or the job finalizes. Requires `.channel(...)` or an
    /// observer to have somewhere to deliver the events.
    ///
    /// Pairs naturally with [`interpret::PhaseModel`](crate::interpret::PhaseModel):
    /// pass a closure that consults the same phase weights.
    pub fn creep<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Option<(f32, f32)> + Send + Sync + 'static,
    {
        self.creep = Some(Box::new(f));
        self
    }

    #[allow(clippy::type_complexity)]
    fn finalize_cmd(
        self,
    ) -> (
        AppHandle<R>,
        Runtime,
        Command,
        Option<String>,
        Vec<EventObserver<R>>,
        Option<CreepFn>,
    ) {
        let cmd = if self.tags.is_empty() {
            self.cmd
        } else {
            self.cmd.tags(self.tags)
        };
        (
            self.app,
            self.rt,
            cmd,
            self.channel,
            self.observers,
            self.creep,
        )
    }

    fn spawn_with_forwarding(self) -> Result<(JobHandle, Option<JoinHandle<()>>), Error> {
        let (app, rt, cmd, channel, observers, creep) = self.finalize_cmd();
        let mut handle = rt.spawn(cmd)?;
        let forwarder = if channel.is_some() || !observers.is_empty() || creep.is_some() {
            Some(forward_events(
                app,
                handle.id(),
                handle.subscribe(),
                channel,
                observers,
                creep,
            ))
        } else {
            None
        };
        Ok((handle, forwarder))
    }

    /// Spawn, return the [`JobHandle`]. If `.channel(name)` was set, a
    /// background task forwards events to `name` until the job finalizes. Any
    /// `.observe(...)` callbacks run on that same forwarding task.
    pub fn spawn(self) -> Result<JobHandle, Error> {
        let (handle, _forwarder) = self.spawn_with_forwarding()?;
        Ok(handle)
    }

    /// Spawn fire-and-forget; return only the [`JobId`]. Use this from
    /// `#[tauri::command]` handlers that hand the id back to the frontend
    /// and rely on the channel for status updates.
    pub fn spawn_tracked(self) -> Result<JobId, Error> {
        let handle = self.spawn()?;
        Ok(handle.id())
    }
}

/// `.await` on the builder runs the job to completion and yields the
/// [`Outcome`]. Spawn failures are surfaced as `Outcome::Failed { SpawnFailed }`.
impl<R: TauriRuntime> IntoFuture for TaskBuilder<R> {
    type Output = Outcome;
    type IntoFuture = Pin<Box<dyn Future<Output = Outcome> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            match self.spawn_with_forwarding() {
                Ok((handle, forwarder)) => {
                    let outcome = handle.await;
                    if let Some(forwarder) = forwarder {
                        let _ = forwarder.await;
                    }
                    outcome
                }
                Err(e) => Outcome::Failed {
                    reason: FailureReason::SpawnFailed {
                        error: e.to_string(),
                    },
                    summary: None,
                    findings: vec![],
                },
            }
        })
    }
}

fn forward_events<R: TauriRuntime>(
    app: AppHandle<R>,
    job_id: JobId,
    mut stream: EventStream,
    channel: Option<String>,
    observers: Vec<EventObserver<R>>,
    creep: Option<CreepFn>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // A free fn, not a closure: a closure capturing `&observers` would
        // have to be held across the `.await` below, and `EventObserver` is
        // `Send` but not `Sync`, so that borrow would make the future non-Send.
        fn deliver<R: TauriRuntime>(
            app: &AppHandle<R>,
            observers: &[EventObserver<R>],
            channel: &Option<String>,
            event: &Event,
        ) {
            for observer in observers {
                observer(app, event);
            }
            if let Some(channel) = channel {
                let _ = app.emit(channel, event);
            }
        }

        // No creep configured: stay on the cheap straight-through path.
        let Some(creep) = creep else {
            while let Some(event) = stream.next().await {
                let stop = matches!(event, Event::Finalized { .. });
                deliver(&app, &observers, &channel, &event);
                if stop {
                    break;
                }
            }
            return;
        };

        let mut state: Option<CreepState> = None;
        let mut ticker = tokio::time::interval(Duration::from_millis(500));
        // First `tick()` resolves immediately; consume it so the first real
        // bump lands ~500 ms after a creep phase opens.
        ticker.tick().await;

        loop {
            tokio::select! {
                biased;
                next = stream.next() => {
                    let Some(event) = next else { break };
                    let stop = matches!(event, Event::Finalized { .. });
                    match &event {
                        // A creep phase opens — arm the ticker for its slice.
                        // A phase with no range (real signal) disarms it.
                        Event::PhaseEntered { name, .. } => {
                            state = creep(name)
                                .filter(|(s, e)| e > s)
                                .map(|(s, e)| CreepState::new(s, e));
                        }
                        Event::PhaseExited { .. }
                        | Event::Finalized { .. }
                        | Event::Cancelled { .. } => state = None,
                        _ => {}
                    }
                    deliver(&app, &observers, &channel, &event);
                    if stop {
                        break;
                    }
                }
                _ = ticker.tick(), if state.is_some() => {
                    let cs = state.as_mut().expect("guarded by state.is_some()");
                    match cs.tick() {
                        Some(frac) => deliver(&app, &observers, &channel, &Event::ProgressUpdated {
                            job: job_id,
                            progress: Progress::fraction(frac),
                            at: SystemTime::now(),
                        }),
                        // Reached the cap — disarm so the timer arm goes
                        // idle until the next creep phase.
                        None if cs.done => state = None,
                        None => {}
                    }
                }
            }
        }
    })
}
