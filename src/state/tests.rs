use super::*;
use crate::model::{
    identity::{HostGeneration, HostSourceId},
    observation::NamespacedId,
    state::{HostState, SourceAvailability, StateRecord},
    time::Timestamp,
};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Wake},
    time::Duration,
};

struct Epochs {
    next: u8,
    fail_at: Option<u8>,
}
impl EpochSource for Epochs {
    fn next(&mut self) -> Result<Epoch> {
        if self.fail_at == Some(self.next) {
            return Err(StateError::Epoch(std::io::Error::other(
                "synthetic entropy failure",
            )));
        }
        let epoch = Epoch::from_bytes([self.next; 16]);
        self.next += 1;
        Ok(epoch)
    }
}
fn epochs() -> Box<dyn EpochSource> {
    Box::new(Epochs {
        next: 1,
        fail_at: None,
    })
}
fn records(id: HostSourceId, generation: HostGeneration, scope: &str) -> Records {
    let record = StateRecord::Host(Box::new(HostState {
        id,
        generation,
        scope: NamespacedId::new("test", scope).unwrap(),
        availability: SourceAvailability::Active,
        fields: BTreeMap::new(),
    }));
    BTreeMap::from([(record.key(), Arc::new(record))])
}
fn next(
    subscription: &mut Subscription,
) -> Option<std::result::Result<Arc<Commit>, subscription::ResyncRequired>> {
    match subscription.poll_next(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("expected queued control or commit"),
    }
}
fn publish(store: &mut StateStore, records: Records, tick: u64) -> Arc<Commit> {
    let cursor = store.reserve().unwrap();
    store
        .publish(cursor, records, Timestamp(Duration::from_secs(tick)))
        .unwrap()
}

#[test]
fn subscribing_between_reservation_and_publication_has_no_bootstrap_gap_or_duplicate_baseline() {
    let id = HostSourceId::new().unwrap();
    let generation = HostGeneration::new().unwrap();
    let initial = records(id, generation, "old");
    let mut store = StateStore::new(initial.clone(), epochs()).unwrap();
    let reader = store.reader();
    let reservation = store.reserve().unwrap();
    let (baseline, mut subscription) = reader.subscribe_and_snapshot(4).unwrap();
    assert_eq!(baseline.cursor.revision, 0);
    assert_eq!(baseline.records, initial);
    assert!(
        subscription
            .poll_next(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    let changed = records(id, generation, "new");
    let commit = store
        .publish(reservation, changed.clone(), Timestamp::ZERO)
        .unwrap();
    assert_eq!(*next(&mut subscription).unwrap().unwrap(), *commit);
    assert_eq!(commit.cursor.revision, baseline.cursor.revision + 1);
    assert_eq!(reader.snapshot().unwrap().records, changed);
}

#[test]
fn full_record_upserts_and_removals_reconstruct_the_current_snapshot() {
    let a = HostSourceId::new().unwrap();
    let b = HostSourceId::new().unwrap();
    let generation = HostGeneration::new().unwrap();
    let mut store = StateStore::new(records(a, generation, "a"), epochs()).unwrap();
    let (mut view, mut sub) = store.reader().subscribe_and_snapshot(8).unwrap();
    let mut both = records(a, generation, "updated-a");
    both.extend(records(b, generation, "b"));
    publish(&mut store, both, 1);
    publish(&mut store, records(b, generation, "updated-b"), 2);
    for expected in 1..=2 {
        let commit = next(&mut sub).unwrap().unwrap();
        assert_eq!(commit.cursor.revision, expected);
        assert!(
            commit
                .upserts
                .keys()
                .all(|key| !commit.removals.contains(key))
        );
        for key in &commit.removals {
            view.records.remove(key);
        }
        view.records.extend(
            commit
                .upserts
                .iter()
                .map(|(key, value)| (*key, value.clone())),
        );
        view.cursor = commit.cursor;
    }
    assert_eq!(view, store.reader().snapshot().unwrap());
}

#[test]
fn overflowing_only_the_slow_subscriber_preserves_consecutive_commits_for_healthy_consumers() {
    let id = HostSourceId::new().unwrap();
    let generation = HostGeneration::new().unwrap();
    let mut store = StateStore::new(records(id, generation, "initial"), epochs()).unwrap();
    let reader = store.reader();
    let (_, mut slow) = reader.subscribe_and_snapshot(1).unwrap();
    let (_, mut healthy) = reader.subscribe_and_snapshot(4).unwrap();
    publish(&mut store, records(id, generation, "first"), 1);
    publish(&mut store, records(id, generation, "second"), 2);
    assert_eq!(
        next(&mut slow).unwrap().unwrap_err().reason,
        ResyncReason::Overflow
    );
    assert!(next(&mut slow).is_none());
    assert_eq!(next(&mut healthy).unwrap().unwrap().cursor.revision, 1);
    assert_eq!(next(&mut healthy).unwrap().unwrap().cursor.revision, 2);
    assert_eq!(reader.inner.lock().unwrap().subscribers.len(), 1);
    let (recovered, _) = reader.subscribe_and_snapshot(2).unwrap();
    assert_eq!(recovered.cursor.revision, 2);
}

#[test]
fn dropping_subscriptions_removes_their_registration_without_retaining_history() {
    let mut store = StateStore::new(Records::new(), epochs()).unwrap();
    let reader = store.reader();
    for _ in 0..32 {
        let (_, subscription) = reader.subscribe_and_snapshot(1).unwrap();
        drop(subscription);
    }
    assert!(reader.inner.lock().unwrap().subscribers.is_empty());
    let reservation = store.reserve().unwrap();
    assert!(
        store
            .publish(reservation, Records::new(), Timestamp::ZERO)
            .is_none()
    );
    assert_eq!(reader.snapshot().unwrap().cursor.revision, 0);
    assert!(reader.subscribe_and_snapshot(0).is_err());
}

struct Reenter {
    reader: StateReader,
    wakes: AtomicUsize,
}
impl Wake for Reenter {
    fn wake(self: Arc<Self>) {
        self.reader.snapshot().unwrap();
        self.wakes.fetch_add(1, Ordering::Relaxed);
    }
}
#[test]
fn subscriber_wakers_can_reenter_snapshot_reads_after_publication_releases_all_locks() {
    let mut store = StateStore::new(Records::new(), epochs()).unwrap();
    let reader = store.reader();
    let (_, mut sub) = reader.subscribe_and_snapshot(1).unwrap();
    let reenter = Arc::new(Reenter {
        reader,
        wakes: AtomicUsize::new(0),
    });
    let waker = Waker::from(reenter.clone());
    assert!(sub.poll_next(&mut Context::from_waker(&waker)).is_pending());
    let id = HostSourceId::new().unwrap();
    let generation = HostGeneration::new().unwrap();
    publish(&mut store, records(id, generation, "scope"), 1);
    assert_eq!(reenter.wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn revision_exhaustion_resets_epoch_without_changing_the_runtime_time_domain() {
    let mut store = StateStore::new(Records::new(), epochs()).unwrap();
    store.force_revision(u64::MAX);
    let reader = store.reader();
    let (old, mut sub) = reader.subscribe_and_snapshot(1).unwrap();
    let id = HostSourceId::new().unwrap();
    let generation = HostGeneration::new().unwrap();
    let commit = publish(&mut store, records(id, generation, "scope"), 1);
    assert_ne!(commit.cursor.epoch, old.cursor.epoch);
    assert_eq!(commit.cursor.revision, 1);
    let snapshot = reader.snapshot().unwrap();
    assert_eq!(snapshot.time_domain, old.time_domain);
    assert_eq!(commit.upserts, snapshot.records);
    assert_eq!(
        next(&mut sub).unwrap().unwrap_err().reason,
        ResyncReason::EpochChanged
    );
    assert!(next(&mut sub).is_none());
}

#[test]
fn entropy_failure_during_rollover_preserves_the_previous_cursor_records_and_subscription() {
    let mut store = StateStore::new(
        Records::new(),
        Box::new(Epochs {
            next: 1,
            fail_at: Some(2),
        }),
    )
    .unwrap();
    store.force_revision(u64::MAX);
    let reader = store.reader();
    let (old, mut sub) = reader.subscribe_and_snapshot(1).unwrap();
    assert!(matches!(store.reserve(), Err(StateError::Epoch(_))));
    assert_eq!(reader.snapshot().unwrap(), old);
    assert!(
        sub.poll_next(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
}

#[test]
fn closing_the_writer_invalidates_live_subscriptions_and_reader_liveness() {
    let store = StateStore::new(Records::new(), epochs()).unwrap();
    let reader = store.reader();
    let (_, mut sub) = reader.subscribe_and_snapshot(1).unwrap();
    drop(store);
    assert_eq!(
        next(&mut sub).unwrap().unwrap_err().reason,
        ResyncReason::Closed
    );
    assert!(next(&mut sub).is_none());
    assert!(reader.snapshot().is_err());
}

#[test]
fn a_repeated_epoch_is_rejected_without_creating_a_reused_cursor() {
    struct Repeated;
    impl EpochSource for Repeated {
        fn next(&mut self) -> Result<Epoch> {
            Ok(Epoch::from_bytes([1; 16]))
        }
    }
    let mut store = StateStore::new(Records::new(), Box::new(Repeated)).unwrap();
    store.force_revision(u64::MAX);
    let before = store.reader().snapshot().unwrap();
    assert!(matches!(store.reserve(), Err(StateError::EpochReused)));
    assert_eq!(store.reader().snapshot().unwrap(), before);
}

#[test]
fn replacing_waiters_drops_user_wakers_outside_subscription_locks() {
    struct DropProbe {
        queue: Weak<Mutex<Queue>>,
        unlocked: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Wake for DropProbe {
        fn wake(self: Arc<Self>) {
            self.unlocked.store(
                self.queue
                    .upgrade()
                    .is_some_and(|queue| queue.try_lock().is_ok()),
                Ordering::Relaxed,
            );
        }
    }
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.unlocked.store(
                self.queue.upgrade().unwrap().try_lock().is_ok(),
                Ordering::Relaxed,
            );
        }
    }
    let store = StateStore::new(Records::new(), epochs()).unwrap();
    let (_, mut sub) = store.reader().subscribe_and_snapshot(1).unwrap();
    let unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waker = Waker::from(Arc::new(DropProbe {
        queue: Arc::downgrade(&sub.queue),
        unlocked: unlocked.clone(),
    }));
    assert!(sub.poll_next(&mut Context::from_waker(&waker)).is_pending());
    drop(waker);
    assert!(
        sub.poll_next(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    assert!(unlocked.load(Ordering::Relaxed));
}
