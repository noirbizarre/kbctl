use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

#[derive(Default)]
pub(crate) struct WakeCounter(pub(crate) AtomicUsize);
impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
pub(crate) fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::from(Arc::new(WakeCounter::default()));
    future.poll(&mut Context::from_waker(&waker))
}
pub(crate) fn ready<F: Future>(future: F) -> F::Output {
    ready_with_budget(future, 256)
}
pub(crate) fn ready_with_budget<F: Future>(future: F, polls: usize) -> F::Output {
    let mut future = Box::pin(future);
    let counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(counter.clone());
    for _ in 0..polls {
        let before = counter.0.load(Ordering::Relaxed);
        if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return value;
        }
        // Only drive immediately-woken work. Blind repeated polling can make a
        // broken Pending implementation pass despite never notifying its host.
        assert!(
            counter.0.load(Ordering::Relaxed) > before,
            "future is pending without an immediate wake; release its scripted gate/input or explicitly repoll after advancing the clock"
        );
    }
    panic!("future exceeded the bounded immediate-progress budget");
}
