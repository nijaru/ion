use super::*;
use std::{
    future::Future,
    pin::pin,
    sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Wake, Waker},
};

struct ReentrantObserver {
    gate: Weak<AdmissionObservations>,
    locked: AtomicBool,
    queued: AtomicBool,
}
impl Wake for ReentrantObserver {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let gate = self.gate.upgrade().unwrap();
        if gate.state.try_lock().is_err() {
            self.locked.store(true, Ordering::SeqCst);
            return;
        }
        if !self.queued.swap(true, Ordering::SeqCst) {
            gate.observer()(AgentEvent::TextDelta {
                block: 0,
                text: "reentrant suffix".into(),
            });
        }
    }
}
#[test]
fn admission_prefix_publication_allows_reentrant_wakes_without_overtaking() {
    let (sender, mut receiver) = broadcast::channel(OBSERVATION_CAPACITY);
    let gate = AdmissionObservations::new(sender, OperationId(7));
    let mut observer = gate.observer();
    observer(AgentEvent::TextDelta {
        block: 0,
        text: "first".into(),
    });
    observer(AgentEvent::TextDelta {
        block: 0,
        text: "second".into(),
    });
    let probe = Arc::new(ReentrantObserver {
        gate: Arc::downgrade(&gate),
        locked: AtomicBool::new(false),
        queued: AtomicBool::new(false),
    });
    {
        let waker = Waker::from(probe.clone());
        let mut waiting = pin!(receiver.recv());
        assert!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        gate.publish();
    }
    assert!(
        !probe.locked.load(Ordering::SeqCst),
        "broadcast wake must not hold the admission gate"
    );
    assert!(
        probe.queued.load(Ordering::SeqCst),
        "exercise a real reentrant observation"
    );
    for text in ["first", "second", "reentrant suffix"] {
        let Observation::Event { event, .. } = receiver.try_recv().unwrap() else {
            panic!("event")
        };
        assert!(matches!(&*event, AgentEvent::TextDelta { text: value, .. } if value == text));
    }
    assert!(receiver.try_recv().is_err());
}

#[test]
fn admission_prefix_retains_gap_and_bounded_ordered_suffix() {
    let (sender, mut receiver) = broadcast::channel(OBSERVATION_CAPACITY);
    let gate = AdmissionObservations::new(sender, OperationId(7));
    let mut observer = gate.observer();
    for index in 0..80 {
        observer(AgentEvent::TextDelta {
            block: 0,
            text: index.to_string(),
        });
    }
    gate.publish();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        Observation::Gap { skipped: 17 }
    ));
    for index in 17..80 {
        let Observation::Event { event, .. } = receiver.try_recv().unwrap() else {
            panic!("event")
        };
        assert!(
            matches!(&*event, AgentEvent::TextDelta { text: value, .. } if value == &index.to_string())
        );
    }
    assert!(receiver.try_recv().is_err());
}
