use super::Inner;
use crate::model::state::Commit;
use miette::Diagnostic;
use std::{
    collections::VecDeque,
    future::poll_fn,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResyncReason {
    Overflow,
    EpochChanged,
    Closed,
}
#[derive(Debug, Clone, Error, Diagnostic, PartialEq, Eq)]
#[error("state subscription requires a fresh snapshot: {reason:?}")]
#[diagnostic(
    code(kbctl::state::resync_required),
    help("Discard the incomplete view and call subscribe_and_snapshot again on a live reader.")
)]
pub(crate) struct ResyncRequired {
    pub(crate) reason: ResyncReason,
}

pub(super) struct Queue {
    pub(super) items: VecDeque<Arc<Commit>>,
    pub(super) capacity: usize,
    terminal: Option<ResyncRequired>,
    pub(super) ended: bool,
    waker: Option<Waker>,
}
impl Queue {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity,
            terminal: None,
            ended: false,
            waker: None,
        }
    }
    pub(super) fn stop(&mut self, reason: ResyncReason) -> Option<Waker> {
        if self.ended {
            return None;
        }
        self.items.clear();
        self.ended = true;
        self.terminal = Some(ResyncRequired { reason });
        self.waker.take()
    }
    pub(super) fn push(&mut self, commit: Arc<Commit>) -> Option<Waker> {
        if self.items.len() == self.capacity {
            return self.stop(ResyncReason::Overflow);
        }
        self.items.push_back(commit);
        self.waker.take()
    }
}

pub(crate) struct Subscription {
    pub(super) queue: Arc<Mutex<Queue>>,
    pub(super) store: Weak<Mutex<Inner>>,
    pub(super) id: u64,
}
impl Subscription {
    pub(crate) fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Arc<Commit>, ResyncRequired>>> {
        // Raw waker clone/drop callbacks are user code too. Keep them outside
        // locks, just like wake(), so waiter replacement cannot invert lock order.
        let next_waker = cx.waker().clone();
        let mut queue = self.queue.lock().expect("state subscription lock poisoned");
        if let Some(error) = queue.terminal.take() {
            return Poll::Ready(Some(Err(error)));
        }
        if let Some(commit) = queue.items.pop_front() {
            return Poll::Ready(Some(Ok(commit)));
        }
        if queue.ended {
            return Poll::Ready(None);
        }
        let old_waker = queue.waker.replace(next_waker);
        drop(queue);
        drop(old_waker);
        Poll::Pending
    }
    pub(crate) async fn next(
        &mut self,
    ) -> Option<std::result::Result<Arc<Commit>, ResyncRequired>> {
        poll_fn(|cx| self.poll_next(cx)).await
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        // Match publisher lock order: store first, queue second. Never retain
        // dead subscribers or invoke a user waker during either critical section.
        if let Some(store) = self.store.upgrade() {
            store
                .lock()
                .expect("state store lock poisoned")
                .subscribers
                .remove(&self.id);
        }
        let mut queue = self.queue.lock().expect("state subscription lock poisoned");
        queue.items.clear();
        queue.ended = true;
        let old_waker = queue.waker.take();
        drop(queue);
        drop(old_waker);
    }
}
