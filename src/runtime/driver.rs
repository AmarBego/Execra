use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::command::{CommandSpec, StdinMode};
use crate::event::{Event, Stream};
use crate::finding::Finding;
use crate::interpreter::{Context as InterpCtx, Interpreter, InterpreterEvent, Line};
use crate::job::{Job, JobId, JobState};
use crate::outcome::{ExitCode, FailureReason, Outcome};
use crate::phase::{Phase, PhaseId};
use crate::proc_group::ProcessGroup;
use crate::store::Store;

use super::raw_output::{finalize_raw_log, open_raw_log};
use super::wait::{spawn_wait_task, WaitOutcome};
use super::{Config, MemoryRawOutput, RawOutputPolicy, MEMORY_RAW_CAPACITY};

struct DriverState {
    phase_stack: Vec<Phase>,
    findings: Vec<Finding>,
    summary: Option<String>,
    known_error: Option<(String, String)>,
    interp_disabled: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn drive_job(
    job_id: JobId,
    spec: CommandSpec,
    stdin: StdinMode,
    mut interpreter: Option<Box<dyn Interpreter + Send + 'static>>,
    cancel: CancellationToken,
    events_tx: broadcast::Sender<Event>,
    outcome_tx: oneshot::Sender<Outcome>,
    snapshot: Arc<Mutex<Job>>,
    store: Store,
    config: Config,
    permits: Arc<tokio::sync::Semaphore>,
    memory_raw: MemoryRawOutput,
) {
    let _permit = match permits.acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return,
    };
    let start = Instant::now();
    let mut raw_file = open_raw_log(job_id, &config).await;

    let mut tcmd = TokioCommand::new(&spec.program);
    tcmd.args(&spec.args);
    if spec.env_clear {
        tcmd.env_clear();
    }
    for (k, v) in &spec.env {
        tcmd.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        tcmd.current_dir(cwd);
    }
    tcmd.stdout(Stdio::piped());
    tcmd.stderr(Stdio::piped());
    tcmd.stdin(match &stdin {
        StdinMode::Null => Stdio::null(),
        StdinMode::Inherit => Stdio::inherit(),
        StdinMode::Piped(_) => Stdio::piped(),
    });
    tcmd.kill_on_drop(true);
    configure_hidden_window(&mut tcmd, spec.hide_window);
    ProcessGroup::configure(&mut tcmd);

    let mut child = match tcmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let outcome = Outcome::Failed {
                reason: FailureReason::SpawnFailed {
                    error: e.to_string(),
                },
                summary: None,
                findings: vec![],
            };
            update_snapshot(&snapshot, |j| {
                j.state = JobState::Finalized;
                j.outcome = Some(outcome.clone());
            });
            persist_snapshot(&store, &snapshot).await;
            finalize_raw_log(job_id, &config, &mut raw_file).await;
            let _ = events_tx.send(Event::Finalized {
                job: job_id,
                outcome: outcome.clone(),
                at: SystemTime::now(),
            });
            let _ = outcome_tx.send(outcome);
            return;
        }
    };

    let pid = child.id().unwrap_or(0);
    let proc_group = ProcessGroup::assign(&child);
    let _ = events_tx.send(Event::JobStarted {
        job: job_id,
        pid,
        at: SystemTime::now(),
    });
    update_snapshot(&snapshot, |j| {
        j.state = JobState::Running;
        j.started_at = Some(SystemTime::now());
    });
    persist_snapshot(&store, &snapshot).await;

    if let StdinMode::Piped(bytes) = stdin {
        if let Some(mut sin) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = sin.write_all(&bytes).await;
                let _ = sin.shutdown().await;
            });
        }
    }

    let mut stdout_lines = child.stdout.take().map(BufReader::new);
    let mut stderr_lines = child.stderr.take().map(BufReader::new);
    let mut exit_rx = spawn_wait_task(
        job_id,
        child,
        proc_group,
        spec.timeout,
        config.default_grace_period,
        cancel.clone(),
        events_tx.clone(),
    );

    let mut state = DriverState {
        phase_stack: Vec::new(),
        findings: Vec::new(),
        summary: None,
        known_error: None,
        interp_disabled: false,
    };

    let mut stdout_done = stdout_lines.is_none();
    let mut stderr_done = stderr_lines.is_none();
    let mut exit_status: Option<std::io::Result<std::process::ExitStatus>> = None;
    let mut child_exited = false;
    let mut timed_out = false;

    while !(stdout_done && stderr_done && child_exited) {
        tokio::select! {
            biased;
            line = next_line(stdout_lines.as_mut()), if !stdout_done => {
                match line {
                    Some(Ok(text)) => process_line(job_id, Stream::Stdout, text, &spec, start, &mut state, &mut interpreter, &events_tx, &snapshot, &mut raw_file, &memory_raw, config.raw_output).await,
                    _ => stdout_done = true,
                }
            }
            line = next_line(stderr_lines.as_mut()), if !stderr_done => {
                match line {
                    Some(Ok(text)) => process_line(job_id, Stream::Stderr, text, &spec, start, &mut state, &mut interpreter, &events_tx, &snapshot, &mut raw_file, &memory_raw, config.raw_output).await,
                    _ => stderr_done = true,
                }
            }
            res = &mut exit_rx, if !child_exited => {
                child_exited = true;
                let ec = match &res {
                    Ok(WaitOutcome { status: Ok(s), .. }) => from_status(s),
                    _ => ExitCode { code: None, signal: None },
                };
                timed_out = res.as_ref().map(|r| r.timed_out).unwrap_or(false);
                exit_status = Some(match res {
                    Ok(outcome) => outcome.status,
                    Err(_) => Err(std::io::Error::other("wait task dropped")),
                });
                let _ = events_tx.send(Event::Exited { job: job_id, code: ec, at: SystemTime::now() });
                update_snapshot(&snapshot, |j| {
                    j.exit = Some(ec);
                    j.state = JobState::Exited;
                });
                persist_snapshot(&store, &snapshot).await;
            }
        }
    }

    let exit_code = match &exit_status {
        Some(Ok(s)) => from_status(s),
        _ => ExitCode {
            code: None,
            signal: None,
        },
    };
    run_on_exit(
        job_id,
        &spec,
        start,
        &mut state,
        &mut interpreter,
        &events_tx,
        &snapshot,
        &exit_code,
    );
    exit_open_phases(job_id, &mut state, &events_tx, &snapshot);

    let outcome = compute_outcome(cancel.is_cancelled(), timed_out, exit_code, state);
    update_snapshot(&snapshot, |j| {
        j.state = JobState::Finalized;
        j.outcome = Some(outcome.clone());
    });
    persist_snapshot(&store, &snapshot).await;
    finalize_raw_log(job_id, &config, &mut raw_file).await;
    let _ = events_tx.send(Event::Finalized {
        job: job_id,
        outcome: outcome.clone(),
        at: SystemTime::now(),
    });
    let _ = outcome_tx.send(outcome);
}

#[allow(clippy::too_many_arguments)]
async fn process_line(
    job_id: JobId,
    stream: Stream,
    text: String,
    spec: &CommandSpec,
    start: Instant,
    state: &mut DriverState,
    interpreter: &mut Option<Box<dyn Interpreter + Send + 'static>>,
    events_tx: &broadcast::Sender<Event>,
    snapshot: &Arc<Mutex<Job>>,
    raw_file: &mut Option<tokio::fs::File>,
    memory_raw: &MemoryRawOutput,
    raw_policy: RawOutputPolicy,
) {
    let at = SystemTime::now();
    if let Some(file) = raw_file.as_mut() {
        let prefix = match stream {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        };
        let _ = file
            .write_all(format!("[{prefix}] {text}\n").as_bytes())
            .await;
    }
    if matches!(raw_policy, RawOutputPolicy::MemoryOnly) {
        let prefix = match stream {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        };
        let mut raw = memory_raw.lock().unwrap();
        let lines = raw.entry(job_id).or_default();
        lines.push_back(format!("[{prefix}] {text}"));
        while lines.len() > MEMORY_RAW_CAPACITY {
            lines.pop_front();
        }
    }
    let _ = events_tx.send(Event::OutputAppended {
        job: job_id,
        stream,
        line: text.clone(),
        at,
    });

    if let Some(interp) = interpreter.as_mut() {
        if !state.interp_disabled {
            let line = Line { stream, text, at };
            let line_for_err = line.text.clone();
            let ctx = InterpCtx {
                job: job_id,
                command: spec,
                current_phase: state.phase_stack.last(),
                phase_stack: &state.phase_stack,
                elapsed: start.elapsed(),
            };
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                interp.on_line(&ctx, &line)
            }));
            match res {
                Ok(evs) => apply_events(evs, job_id, state, events_tx, snapshot),
                Err(p) => {
                    state.interp_disabled = true;
                    let _ = events_tx.send(Event::InterpreterError {
                        job: job_id,
                        interpreter: "interpreter".into(),
                        error: panic_msg(&p),
                        line: Some(line_for_err),
                        at: SystemTime::now(),
                    });
                }
            }
        }
    }
}

fn run_on_exit(
    job_id: JobId,
    spec: &CommandSpec,
    start: Instant,
    state: &mut DriverState,
    interpreter: &mut Option<Box<dyn Interpreter + Send + 'static>>,
    events_tx: &broadcast::Sender<Event>,
    snapshot: &Arc<Mutex<Job>>,
    exit_code: &ExitCode,
) {
    if let Some(interp) = interpreter.as_mut() {
        if !state.interp_disabled {
            let ctx = InterpCtx {
                job: job_id,
                command: spec,
                current_phase: state.phase_stack.last(),
                phase_stack: &state.phase_stack,
                elapsed: start.elapsed(),
            };
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                interp.on_exit(&ctx, exit_code)
            }));
            match res {
                Ok(evs) => apply_events(evs, job_id, state, events_tx, snapshot),
                Err(p) => {
                    state.interp_disabled = true;
                    let _ = events_tx.send(Event::InterpreterError {
                        job: job_id,
                        interpreter: "interpreter".into(),
                        error: panic_msg(&p),
                        line: None,
                        at: SystemTime::now(),
                    });
                }
            }
        }
    }
}

fn apply_events(
    evs: Vec<InterpreterEvent>,
    job_id: JobId,
    state: &mut DriverState,
    events_tx: &broadcast::Sender<Event>,
    snapshot: &Arc<Mutex<Job>>,
) {
    for ev in evs {
        let at = SystemTime::now();
        match ev {
            InterpreterEvent::EnterPhase { name, label } => {
                let phase = Phase {
                    id: PhaseId::new(),
                    name: name.clone(),
                    label: label.clone(),
                    entered_at: at,
                };
                let id = phase.id;
                state.phase_stack.push(phase);
                update_snapshot(snapshot, |j| j.current_phase = Some(id));
                let _ = events_tx.send(Event::PhaseEntered {
                    job: job_id,
                    phase: id,
                    name,
                    label,
                    at,
                });
            }
            InterpreterEvent::UpdatePhase { label } => {
                if let Some(top) = state.phase_stack.last_mut() {
                    top.label = Some(label.clone());
                    let _ = events_tx.send(Event::PhaseUpdated {
                        job: job_id,
                        phase: top.id,
                        label,
                        at,
                    });
                } else {
                    emit_interp_error(
                        job_id,
                        "UpdatePhase with empty phase stack",
                        None,
                        at,
                        events_tx,
                    );
                }
            }
            InterpreterEvent::ExitPhase => {
                if let Some(top) = state.phase_stack.pop() {
                    let new_top = state.phase_stack.last().map(|p| p.id);
                    update_snapshot(snapshot, |j| j.current_phase = new_top);
                    let _ = events_tx.send(Event::PhaseExited {
                        job: job_id,
                        phase: top.id,
                        at,
                    });
                } else {
                    emit_interp_error(
                        job_id,
                        "ExitPhase with empty phase stack",
                        None,
                        at,
                        events_tx,
                    );
                }
            }
            InterpreterEvent::Progress { progress } => {
                let p = progress.normalize();
                update_snapshot(snapshot, |j| j.progress = p.clone());
                let _ = events_tx.send(Event::ProgressUpdated {
                    job: job_id,
                    progress: p,
                    at,
                });
            }
            InterpreterEvent::Label { text } => {
                update_snapshot(snapshot, |j| j.label = Some(text.clone()));
                let _ = events_tx.send(Event::LabelUpdated {
                    job: job_id,
                    label: text,
                    at,
                });
            }
            InterpreterEvent::Warning { code, message } => {
                let _ = events_tx.send(Event::WarningDetected {
                    job: job_id,
                    code,
                    message,
                    at,
                });
            }
            InterpreterEvent::KnownError { code, message } => {
                state.known_error = Some((code.clone(), message.clone()));
                let _ = events_tx.send(Event::KnownErrorDetected {
                    job: job_id,
                    code,
                    message,
                    at,
                });
            }
            InterpreterEvent::Finding { finding } => {
                state.findings.push(finding.clone());
                let _ = events_tx.send(Event::FindingEmitted {
                    job: job_id,
                    finding,
                    at,
                });
            }
            InterpreterEvent::Prompt { prompt } => {
                let _ = events_tx.send(Event::PromptDetected {
                    job: job_id,
                    prompt,
                    at,
                });
            }
            InterpreterEvent::Summary { text } => state.summary = Some(text),
        }
    }
}

fn exit_open_phases(
    job_id: JobId,
    state: &mut DriverState,
    events_tx: &broadcast::Sender<Event>,
    snapshot: &Arc<Mutex<Job>>,
) {
    while let Some(top) = state.phase_stack.pop() {
        let _ = events_tx.send(Event::PhaseExited {
            job: job_id,
            phase: top.id,
            at: SystemTime::now(),
        });
    }
    update_snapshot(snapshot, |j| j.current_phase = None);
}

fn compute_outcome(
    was_cancelled: bool,
    timed_out: bool,
    exit_code: ExitCode,
    state: DriverState,
) -> Outcome {
    if was_cancelled {
        Outcome::Cancelled {
            findings: state.findings,
        }
    } else if timed_out {
        Outcome::Failed {
            reason: FailureReason::Timeout,
            summary: state.summary,
            findings: state.findings,
        }
    } else if exit_code.is_success() {
        Outcome::Succeeded {
            summary: state.summary,
            findings: state.findings,
        }
    } else {
        let reason = if let Some((code, msg)) = state.known_error {
            FailureReason::KnownError { code, message: msg }
        } else if let Some(sig) = exit_code.signal {
            FailureReason::Signal { signal: sig }
        } else if let Some(c) = exit_code.code {
            FailureReason::NonZeroExit { code: c }
        } else {
            FailureReason::NonZeroExit { code: -1 }
        };
        Outcome::Failed {
            reason,
            summary: state.summary,
            findings: state.findings,
        }
    }
}

async fn next_line<R>(lines: Option<&mut BufReader<R>>) -> Option<std::io::Result<String>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    match lines {
        Some(reader) => {
            let mut bytes = Vec::new();
            match reader.read_until(b'\n', &mut bytes).await {
                Ok(0) => None,
                Ok(_) => {
                    if bytes.ends_with(b"\n") {
                        bytes.pop();
                    }
                    if bytes.ends_with(b"\r") {
                        bytes.pop();
                    }
                    Some(Ok(String::from_utf8_lossy(&bytes).into_owned()))
                }
                Err(e) => Some(Err(e)),
            }
        }
        None => None,
    }
}

fn update_snapshot<F: FnOnce(&mut Job)>(snapshot: &Arc<Mutex<Job>>, f: F) {
    if let Ok(mut g) = snapshot.lock() {
        f(&mut g);
    }
}

async fn persist_snapshot(store: &Store, snapshot: &Arc<Mutex<Job>>) {
    let job = match snapshot.lock() {
        Ok(g) => g.clone(),
        Err(_) => return,
    };
    let _ = store.upsert_job(&job).await;
}

#[cfg(windows)]
fn configure_hidden_window(cmd: &mut TokioCommand, hide: bool) {
    if hide {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

#[cfg(not(windows))]
fn configure_hidden_window(_cmd: &mut TokioCommand, _hide: bool) {}

pub(super) fn spawn_store_writer(store: Store, mut rx: broadcast::Receiver<Event>) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = store.insert_event(&event).await;
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });
}

fn emit_interp_error(
    job: JobId,
    error: impl Into<String>,
    line: Option<String>,
    at: SystemTime,
    events_tx: &broadcast::Sender<Event>,
) {
    let _ = events_tx.send(Event::InterpreterError {
        job,
        interpreter: "interpreter".into(),
        error: error.into(),
        line,
        at,
    });
}

fn from_status(s: &std::process::ExitStatus) -> ExitCode {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return ExitCode {
                code: None,
                signal: Some(sig),
            };
        }
    }
    ExitCode {
        code: s.code(),
        signal: None,
    }
}

fn panic_msg(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "interpreter panicked".to_string()
    }
}
