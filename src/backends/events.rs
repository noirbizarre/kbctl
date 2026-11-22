//! Single-consumer, bounded, cancellation-safe delivery without a runtime worker.

use std::{
    collections::VecDeque,
    future::poll_fn,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use super::{
    ObservationDelivery,
    error::{BackendError, FailureKind, Result},
};

struct Queue {
    items: VecDeque<ObservationDelivery>,
    capacity: usize,
    terminal: Option<BackendError>,
    ended: bool,
    waker: Option<Waker>,
    backend: String,
}

#[derive(Clone)]
pub(super) struct Publisher(Arc<Mutex<Queue>>);
pub(crate) struct EventReceiver(Arc<Mutex<Queue>>);

pub(super) struct Publication {
    pub(super) discarded: bool,
    wake: Option<Waker>,
}
impl Publication {
    pub(super) fn wake(self) {
        if let Some(waker) = self.wake {
            waker.wake();
        }
    }
}

pub(super) fn channel(capacity: usize, backend: String) -> Result<(Publisher, EventReceiver)> {
    if capacity == 0 {
        return Err(BackendError::new(
            FailureKind::InvalidInput,
            "event capacity must be nonzero",
        ));
    }
    let shared = Arc::new(Mutex::new(Queue {
        items: VecDeque::new(),
        capacity,
        terminal: None,
        ended: false,
        waker: None,
        backend,
    }));
    Ok((Publisher(shared.clone()), EventReceiver(shared)))
}

impl Publisher {
    /// Report every loss, including input after subscription termination: a read
    /// can recover current data, but a subsequent discarded update invalidates it.
    pub(super) fn publish(&self, event: ObservationDelivery) -> Publication {
        let mut discarded = false;
        let wake = {
            let mut queue = self.0.lock().expect("event queue lock poisoned");
            if queue.ended {
                return Publication {
                    discarded: true,
                    wake: None,
                };
            }
            if queue.items.len() == queue.capacity {
                // Store control state outside the full queue: continuity loss
                // must never be silently hidden behind dropped observations.
                queue.items.clear();
                queue.ended = true;
                discarded = true;
                queue.terminal = Some(
                    BackendError::new(
                        FailureKind::ResyncRequired,
                        "backend event queue overflowed",
                    )
                    .context(
                        &queue.backend,
                        super::error::Phase::Events,
                        None,
                        Some(event.source),
                    ),
                );
            } else {
                queue.items.push_back(event);
            }
            queue.waker.take()
        };
        // A waker may immediately re-enter a consumer; never call it under lock.
        Publication { discarded, wake }
    }
    pub(super) fn finish(&self, error: Option<BackendError>) {
        let wake = {
            let mut queue = self.0.lock().expect("event queue lock poisoned");
            if queue.ended {
                return;
            }
            queue.items.clear();
            queue.ended = true;
            queue.terminal = error;
            queue.waker.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }
}

impl EventReceiver {
    pub(crate) fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<ObservationDelivery>>> {
        let next_waker = cx.waker().clone();
        let mut queue = self.0.lock().expect("event queue lock poisoned");
        if let Some(error) = queue.terminal.take() {
            return Poll::Ready(Some(Err(error)));
        }
        if let Some(event) = queue.items.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }
        if queue.ended {
            return Poll::Ready(None);
        }
        let old_waker = queue.waker.replace(next_waker);
        drop(queue);
        drop(old_waker);
        Poll::Pending
    }
    pub(crate) async fn next(&mut self) -> Option<Result<ObservationDelivery>> {
        poll_fn(|cx| self.poll_next(cx)).await
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        let mut queue = self.0.lock().expect("event queue lock poisoned");
        queue.items.clear();
        queue.ended = true;
        let old_waker = queue.waker.take();
        drop(queue);
        drop(old_waker);
    }
}
