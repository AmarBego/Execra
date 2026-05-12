use std::time::{Duration, SystemTime};

use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::event::Event;
use crate::job::JobId;
use crate::proc_group::ProcessGroup;
use crate::store::Store;

use super::driver::emit;

pub(super) struct WaitOutcome {
    pub(super) status: std::io::Result<std::process::ExitStatus>,
    pub(super) timed_out: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_wait_task(
    job_id: JobId,
    mut child: tokio::process::Child,
    proc_group: Option<ProcessGroup>,
    timeout: Option<Duration>,
    grace_period: Duration,
    cancel: CancellationToken,
    events_tx: broadcast::Sender<Event>,
    store: Option<Store>,
) -> oneshot::Receiver<WaitOutcome> {
    let (exit_tx, exit_rx) = oneshot::channel();
    tokio::spawn(async move {
        let outcome = if let Some(timeout) = timeout {
            tokio::select! {
                r = child.wait() => WaitOutcome { status: r, timed_out: false },
                _ = cancel.cancelled() => {
                    emit(&events_tx, store.as_ref(), Event::Cancelled { job: job_id, at: SystemTime::now() });
                    let status = terminate_with_grace(&mut child, &proc_group, grace_period).await;
                    WaitOutcome { status, timed_out: false }
                }
                _ = tokio::time::sleep(timeout) => {
                    kill_child(&mut child, &proc_group);
                    WaitOutcome { status: child.wait().await, timed_out: true }
                }
            }
        } else {
            tokio::select! {
                r = child.wait() => WaitOutcome { status: r, timed_out: false },
                _ = cancel.cancelled() => {
                    emit(&events_tx, store.as_ref(), Event::Cancelled { job: job_id, at: SystemTime::now() });
                    let status = terminate_with_grace(&mut child, &proc_group, grace_period).await;
                    WaitOutcome { status, timed_out: false }
                }
            }
        };
        let _ = exit_tx.send(outcome);
        drop(proc_group);
    });
    exit_rx
}

async fn terminate_with_grace(
    child: &mut tokio::process::Child,
    proc_group: &Option<ProcessGroup>,
    grace_period: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    terminate_child(child, proc_group);
    tokio::select! {
        status = child.wait() => status,
        _ = tokio::time::sleep(grace_period) => {
            kill_child(child, proc_group);
            child.wait().await
        }
    }
}

fn terminate_child(child: &mut tokio::process::Child, proc_group: &Option<ProcessGroup>) {
    if let Some(pg) = proc_group {
        pg.terminate();
    } else {
        let _ = child.start_kill();
    }
}

fn kill_child(child: &mut tokio::process::Child, proc_group: &Option<ProcessGroup>) {
    if let Some(pg) = proc_group {
        pg.kill();
    } else {
        let _ = child.start_kill();
    }
}
