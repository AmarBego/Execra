use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use tokio::sync::broadcast;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use tokio_stream::StreamExt;

use crate::event::Event;
use crate::job::JobId;

pub struct EventStream {
    rx: BroadcastStream<Event>,
    pub(super) filter: Option<JobId>,
}

impl EventStream {
    pub(super) fn new(rx: broadcast::Receiver<Event>, filter: Option<JobId>) -> Self {
        EventStream {
            rx: BroadcastStream::new(rx),
            filter,
        }
    }
}

impl EventStream {
    /// Returns the next event, or `None` when the channel closes.
    /// Lagged events are silently skipped; callers that need a complete
    /// history should query persisted state via `Execra::job`.
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            match self.rx.next().await {
                Some(Ok(ev)) => {
                    if let Some(f) = self.filter {
                        if ev.job_id() != f {
                            continue;
                        }
                    }
                    return Some(ev);
                }
                Some(Err(BroadcastStreamRecvError::Lagged(_))) => continue,
                None => return None,
            }
        }
    }
}

impl Stream for EventStream {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.rx).poll_next(cx) {
                Poll::Ready(Some(Ok(ev))) => {
                    if let Some(f) = this.filter {
                        if ev.job_id() != f {
                            continue;
                        }
                    }
                    return Poll::Ready(Some(ev));
                }
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(_)))) => continue,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
