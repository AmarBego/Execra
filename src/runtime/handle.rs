use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use tokio::sync::{broadcast, oneshot};

use crate::event::Event;
use crate::job::{JobId, JobState};
use crate::outcome::Outcome;

use super::{Error, EventStream, Inner};

pub struct JobHandle {
    pub(super) id: JobId,
    pub(super) inner: Arc<Inner>,
    pub(super) outcome_rx: Option<oneshot::Receiver<Outcome>>,
    /// Held so the first `subscribe()` can return a receiver that includes
    /// events emitted between spawn and the user calling subscribe.
    pub(super) initial_rx: Option<broadcast::Receiver<Event>>,
}

impl JobHandle {
    pub fn id(&self) -> JobId {
        self.id
    }

    pub fn cancel(&self) -> Result<(), Error> {
        let jobs = self.inner.jobs.lock().unwrap();
        let entry = jobs.get(&self.id).ok_or(Error::UnknownJob(self.id))?;
        if entry.snapshot.lock().unwrap().state == JobState::Finalized {
            return Ok(());
        }
        entry.cancel.cancel();
        Ok(())
    }

    pub fn subscribe(&mut self) -> EventStream {
        let rx = self
            .initial_rx
            .take()
            .unwrap_or_else(|| self.inner.events_tx.subscribe());
        EventStream::new(rx, Some(self.id))
    }
}

impl Future for JobHandle {
    type Output = Outcome;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(rx) = this.outcome_rx.as_mut() {
            match Pin::new(rx).poll(cx) {
                Poll::Ready(Ok(o)) => {
                    this.outcome_rx = None;
                    Poll::Ready(o)
                }
                Poll::Ready(Err(_)) => {
                    this.outcome_rx = None;
                    Poll::Ready(Outcome::Cancelled { findings: vec![] })
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}
