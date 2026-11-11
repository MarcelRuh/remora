//! Ereignis-Brücke von Hintergrund-Threads in den UI-Event-Loop.
//!
//! Sitzungs-Threads dürfen die (nicht `Send`) Slint-Objekte nicht anfassen. Stattdessen
//! werden Ereignisse in eine Warteschlange gestellt; der UI-Thread wird über
//! `slint::invoke_from_event_loop` geweckt und arbeitet alle wartenden Ereignisse gesammelt ab.
//! Mehrere Ereignisse zwischen zwei Weckrufen führen so zu nur einem Durchlauf.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::rdp::{SessionEvent, SessionEventSink, SessionId};

#[derive(Debug)]
pub enum AppEvent {
    Session(SessionId, SessionEvent),
}

type Waker = Box<dyn Fn() + Send + Sync>;

pub struct EventBus {
    queue: Mutex<VecDeque<AppEvent>>,
    wake_pending: AtomicBool,
    waker: Waker,
}

impl EventBus {
    /// `waker` wird aufgerufen, wenn neue Ereignisse vorliegen und noch kein Weckruf aussteht.
    pub fn new(waker: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            wake_pending: AtomicBool::new(false),
            waker: Box::new(waker),
        })
    }

    pub fn publish(&self, event: AppEvent) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push_back(event);
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            (self.waker)();
        }
    }

    /// Entnimmt alle wartenden Ereignisse (nur im UI-Thread aufrufen).
    pub fn drain(&self) -> Vec<AppEvent> {
        self.wake_pending.store(false, Ordering::Release);
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.drain(..).collect()
    }
}

/// Adapter: Sitzungsereignisse landen im Bus.
pub struct BusSink(pub Arc<EventBus>);

impl SessionEventSink for BusSink {
    fn emit(&self, session: SessionId, event: SessionEvent) {
        self.0.publish(AppEvent::Session(session, event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdp::SessionState;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn coalesces_wakeups() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let bus = EventBus::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        for _ in 0..5 {
            bus.publish(AppEvent::Session(1, SessionEvent::State(SessionState::Connecting)));
        }
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(bus.drain().len(), 5);
        bus.publish(AppEvent::Session(1, SessionEvent::LoggedOn));
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
    }
}
