//! Single writer, immutable snapshots and atomic bootstrap; no background task.
#![allow(dead_code)]

use crate::model::{
    state::{Commit, Cursor, Epoch, Records, Result, Snapshot, StateError},
    time::Timestamp,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak},
    task::Waker,
};

pub(crate) mod subscription;
use subscription::{Queue, ResyncReason, Subscription};

pub(crate) trait EpochSource: Send {
    fn next(&mut self) -> Result<Epoch>;
}
pub(crate) struct OsEpochs;
impl EpochSource for OsEpochs {
    fn next(&mut self) -> Result<Epoch> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|error| StateError::Epoch(error.into()))?;
        Ok(Epoch::from_bytes(bytes))
    }
}

struct Inner {
    cursor: Cursor,
    time_domain: Epoch,
    records: Records,
    subscribers: BTreeMap<u64, Weak<Mutex<Queue>>>,
    next_subscriber: u64,
    closed: bool,
}
#[derive(Clone)]
pub(crate) struct StateReader {
    inner: Arc<Mutex<Inner>>,
}
pub(crate) struct StateStore {
    reader: StateReader,
    epochs: Box<dyn EpochSource>,
}
pub(crate) struct Reservation {
    base: Cursor,
    next: Cursor,
}

fn snapshot(inner: &Inner) -> Snapshot {
    Snapshot {
        cursor: inner.cursor,
        time_domain: inner.time_domain,
        records: inner.records.clone(),
    }
}
impl StateReader {
    pub(crate) fn snapshot(&self) -> Result<Snapshot> {
        let inner = self.inner.lock().expect("state store lock poisoned");
        if inner.closed {
            return Err(StateError::Closed);
        }
        Ok(snapshot(&inner))
    }
    pub(crate) fn subscribe_and_snapshot(
        &self,
        capacity: usize,
    ) -> Result<(Snapshot, Subscription)> {
        if capacity == 0 {
            return Err(StateError::Invalid("subscription capacity must be nonzero"));
        }
        let mut inner = self.inner.lock().expect("state store lock poisoned");
        if inner.closed {
            return Err(StateError::Closed);
        }
        let id = inner.next_subscriber;
        let next = id
            .checked_add(1)
            .ok_or(StateError::Invalid("subscriber identities exhausted"))?;
        let queue = Arc::new(Mutex::new(Queue::new(capacity)));
        // Registration and baseline capture are serialized with publication.
        inner.subscribers.insert(id, Arc::downgrade(&queue));
        inner.next_subscriber = next;
        let baseline = snapshot(&inner);
        Ok((
            baseline,
            Subscription {
                queue,
                store: Arc::downgrade(&self.inner),
                id,
            },
        ))
    }
}
impl StateStore {
    pub(crate) fn new(records: Records, mut epochs: Box<dyn EpochSource>) -> Result<Self> {
        let epoch = epochs.next()?;
        Ok(Self {
            reader: StateReader {
                inner: Arc::new(Mutex::new(Inner {
                    cursor: Cursor { epoch, revision: 0 },
                    time_domain: epoch,
                    records,
                    subscribers: BTreeMap::new(),
                    next_subscriber: 1,
                    closed: false,
                })),
            },
            epochs,
        })
    }
    pub(crate) fn reader(&self) -> StateReader {
        self.reader.clone()
    }
    /// Allocate any replacement epoch before the runtime mutates topology.
    pub(crate) fn reserve(&mut self) -> Result<Reservation> {
        let base = self
            .reader
            .inner
            .lock()
            .expect("state store lock poisoned")
            .cursor;
        let next = if let Some(revision) = base.revision.checked_add(1) {
            Cursor {
                epoch: base.epoch,
                revision,
            }
        } else {
            let epoch = self.epochs.next()?;
            if epoch == base.epoch {
                return Err(StateError::EpochReused);
            }
            Cursor { epoch, revision: 1 }
        };
        Ok(Reservation { base, next })
    }
    pub(crate) fn publish(
        &mut self,
        reservation: Reservation,
        records: Records,
        at: Timestamp,
    ) -> Option<Arc<Commit>> {
        let mut wakes = Vec::new();
        let commit = {
            let mut inner = self.reader.inner.lock().expect("state store lock poisoned");
            assert_eq!(
                inner.cursor, reservation.base,
                "only the exclusive runtime writer may publish a reserved cursor"
            );
            let mut upserts: Records = records
                .iter()
                .filter(|(key, value)| inner.records.get(key) != Some(*value))
                .map(|(key, value)| (*key, value.clone()))
                .collect();
            let mut removals: BTreeSet<_> = inner
                .records
                .keys()
                .filter(|key| !records.contains_key(key))
                .copied()
                .collect();
            if upserts.is_empty() && removals.is_empty() {
                return None;
            }
            let rollover = reservation.next.epoch != inner.cursor.epoch;
            if rollover {
                upserts = records.clone();
                removals.clear();
            }
            let commit = Arc::new(Commit {
                cursor: reservation.next,
                at,
                upserts,
                removals,
            });
            inner.cursor = reservation.next;
            inner.records = records;
            inner.subscribers.retain(|_, weak| {
                let Some(queue) = weak.upgrade() else {
                    return false;
                };
                let mut queue = queue.lock().expect("state subscription lock poisoned");
                let wake = if rollover {
                    queue.stop(ResyncReason::EpochChanged)
                } else {
                    queue.push(commit.clone())
                };
                if let Some(waker) = wake {
                    wakes.push(waker);
                }
                !queue.ended
            });
            commit
        };
        wake(wakes);
        Some(commit)
    }
    #[cfg(test)]
    pub(crate) fn force_revision(&mut self, revision: u64) {
        self.reader.inner.lock().unwrap().cursor.revision = revision;
    }
}
fn wake(wakes: Vec<Waker>) {
    for waker in wakes {
        waker.wake();
    }
}
impl Drop for StateStore {
    fn drop(&mut self) {
        let mut wakes = Vec::new();
        {
            let mut inner = self.reader.inner.lock().expect("state store lock poisoned");
            inner.closed = true;
            for weak in inner.subscribers.values() {
                if let Some(queue) = weak.upgrade()
                    && let Some(waker) = queue
                        .lock()
                        .expect("state subscription lock poisoned")
                        .stop(ResyncReason::Closed)
                {
                    wakes.push(waker);
                }
            }
            inner.subscribers.clear();
        }
        wake(wakes);
    }
}

#[cfg(test)]
mod tests;
