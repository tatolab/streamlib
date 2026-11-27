// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The thread each MoQ node's native half runs on, the signal that stops it, and the one thread
//! of this process holding each node's services.
//!
//! A node's services admit one holder, as a device does. A thread started for a node whose
//! services a thread of this process holds takes them from it: the thread before is stopped and
//! waited for first, so the two never hold them at once, and the node that started it later
//! finds it already ended.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::{MoqModuleError, Result};

/// A flag set once, which a thread may wait on. Only a bool is set under its lock, so a
/// poisoned lock still holds a flag worth reading.
#[derive(Default)]
struct OneShotLatch {
    set: Mutex<bool>,
    changed: Condvar,
}

impl OneShotLatch {
    fn set(&self) {
        *self.set.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn is_set(&self) -> bool {
        *self.set.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait up to `timeout` for the flag; whether it is set.
    fn wait_up_to(&self, timeout: Duration) -> bool {
        let set = self.set.lock().unwrap_or_else(PoisonError::into_inner);
        *self
            .changed
            .wait_timeout_while(set, timeout, |set| !*set)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }
}

/// Asked once, when the node stops; its thread checks it between passes or
/// waits on it.
#[derive(Default)]
pub struct MoqNodeThreadStopSignal {
    stop_requested: OneShotLatch,
    ended: OneShotLatch,
}

impl MoqNodeThreadStopSignal {
    /// Whether the node asked its thread to stop.
    pub fn is_requested(&self) -> bool {
        self.stop_requested.is_set()
    }

    /// Wait up to `timeout` for a stop; whether one was asked.
    pub fn wait_for_a_stop_up_to(&self, timeout: Duration) -> bool {
        self.stop_requested.wait_up_to(timeout)
    }
}

/// Marks its thread ended once everything its body held is dropped.
struct MarkedEndedWhenDropped(Arc<MoqNodeThreadStopSignal>);

impl Drop for MarkedEndedWhenDropped {
    fn drop(&mut self) {
        self.0.ended.set();
    }
}

/// The thread holding one node's services, if any; locked for the whole of a takeover, so two
/// threads started for one node take the services in turn.
type HoldersSlot = Arc<Mutex<Option<Arc<MoqNodeThreadStopSignal>>>>;

/// The slot of each node's services in this process, by the name of the service that names its
/// holder. Locked only to find a slot, never across a takeover.
static THE_SLOT_OF_EACH_NODES_SERVICES: LazyLock<Mutex<HashMap<String, HoldersSlot>>> =
    LazyLock::new(Mutex::default);

fn the_slot_of(holding: &str) -> HoldersSlot {
    Arc::clone(
        THE_SLOT_OF_EACH_NODES_SERVICES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(holding.to_owned())
            .or_default(),
    )
}

/// A node's running thread, stopped and joined by its node.
pub struct MoqNodeThread {
    stop_signal: Arc<MoqNodeThreadStopSignal>,
    holding: Option<HoldersSlot>,
    thread: Option<JoinHandle<()>>,
}

impl MoqNodeThread {
    /// Run `body` on a thread named `thread_name` until it returns, handing it
    /// the signal its node stops it with.
    pub fn spawn(
        thread_name: &str,
        body: impl FnOnce(&MoqNodeThreadStopSignal) + Send + 'static,
    ) -> Result<Self> {
        let stop_signal = Arc::new(MoqNodeThreadStopSignal::default());
        let marked_ended_when_dropped = MarkedEndedWhenDropped(Arc::clone(&stop_signal));
        let thread = std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let marked_ended_when_dropped = marked_ended_when_dropped;
                body(&marked_ended_when_dropped.0);
            })
            .map_err(|refusal| MoqModuleError::Relay {
                what: format!("the `{thread_name}` thread did not start: {refusal}"),
            })?;
        Ok(Self {
            stop_signal,
            holding: None,
            thread: Some(thread),
        })
    }

    /// Run `body` on what `open` opens, on a thread named `thread_name`, as the one thread of
    /// this process holding the services `holding` names: a thread holding them before is
    /// stopped, and waited up to `taken_over_within` for, before `open` runs. Refused, opening
    /// nothing, when that thread does not end in time.
    pub fn spawn_holding<Opened: Send + 'static>(
        holding: &str,
        thread_name: &str,
        taken_over_within: Duration,
        open: impl FnOnce() -> Result<Opened>,
        body: impl FnOnce(Opened, &MoqNodeThreadStopSignal) + Send + 'static,
    ) -> Result<Self> {
        let slot = the_slot_of(holding);
        let mut holder = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(holder_before) = holder.as_ref() {
            holder_before.stop_requested.set();
            if !holder_before.ended.wait_up_to(taken_over_within) {
                return Err(MoqModuleError::HolderDidNotEnd {
                    holding: holding.to_owned(),
                    within: taken_over_within,
                });
            }
        }
        let opened = open()?;
        let mut thread = Self::spawn(thread_name, move |stop_signal| body(opened, stop_signal))?;
        *holder = Some(Arc::clone(&thread.stop_signal));
        drop(holder);
        thread.holding = Some(slot);
        Ok(thread)
    }

    /// Stop the thread and wait up to `timeout` for it; whether it ended. A thread a later one
    /// took its services from has ended already.
    pub fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.stop_signal.stop_requested.set();
        let Some(thread) = self.thread.take() else {
            return true;
        };
        if !self.stop_signal.ended.wait_up_to(timeout) {
            self.thread = Some(thread);
            return false;
        }
        let _ = thread.join();
        if let Some(slot) = self.holding.take() {
            let mut holder = slot.lock().unwrap_or_else(PoisonError::into_inner);
            if holder
                .as_ref()
                .is_some_and(|holder| Arc::ptr_eq(holder, &self.stop_signal))
            {
                *holder = None;
            }
        }
        true
    }
}

impl Drop for MoqNodeThread {
    fn drop(&mut self) {
        self.stop_and_join(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;

    use super::*;

    #[test]
    fn a_thread_waiting_on_its_stop_signal_ends_when_stopped() {
        let mut thread = MoqNodeThread::spawn("waits", |stop_signal| {
            while !stop_signal.wait_for_a_stop_up_to(Duration::from_secs(60)) {}
        })
        .unwrap();
        assert!(thread.stop_and_join(Duration::from_secs(5)));
    }

    #[test]
    fn a_thread_that_does_not_end_in_time_is_reported_and_joined_later() {
        let (release, released) = channel::<()>();
        let mut thread = MoqNodeThread::spawn("held", move |_| {
            let _ = released.recv();
        })
        .unwrap();
        assert!(!thread.stop_and_join(Duration::from_millis(20)));
        release.send(()).unwrap();
        assert!(thread.stop_and_join(Duration::from_secs(5)));
    }

    /// A stand-in for a node's services, which admit one holder at a time.
    struct OneHolderServices {
        holders: AtomicUsize,
    }

    struct HeldServices<'services>(&'services OneHolderServices);

    impl<'services> HeldServices<'services> {
        fn open(services: &'services OneHolderServices) -> Result<Self> {
            if services.holders.fetch_add(1, Ordering::SeqCst) != 0 {
                services.holders.fetch_sub(1, Ordering::SeqCst);
                return Err(MoqModuleError::Ipc {
                    what: "two holders".into(),
                });
            }
            Ok(Self(services))
        }
    }

    impl Drop for HeldServices<'_> {
        fn drop(&mut self) {
            self.0.holders.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn a_thread_holding(services: &'static OneHolderServices) -> Result<MoqNodeThread> {
        MoqNodeThread::spawn_holding(
            "test/one-holder",
            "holds",
            Duration::from_secs(5),
            || HeldServices::open(services),
            |held, stop_signal| {
                let _held = held;
                while !stop_signal.wait_for_a_stop_up_to(Duration::from_secs(60)) {}
            },
        )
    }

    #[test]
    fn a_thread_started_for_held_services_takes_them_from_the_thread_before() {
        static SERVICES: OneHolderServices = OneHolderServices {
            holders: AtomicUsize::new(0),
        };
        let mut first = a_thread_holding(&SERVICES).expect("the first thread opens them");
        let mut second = a_thread_holding(&SERVICES)
            .expect("the second thread takes them from the first, never opening them beside it");
        assert_eq!(SERVICES.holders.load(Ordering::SeqCst), 1);
        assert!(
            first.stop_and_join(Duration::ZERO),
            "the first has ended already"
        );
        assert_eq!(SERVICES.holders.load(Ordering::SeqCst), 1);
        assert!(second.stop_and_join(Duration::from_secs(5)));
        assert_eq!(SERVICES.holders.load(Ordering::SeqCst), 0);
        let mut third = a_thread_holding(&SERVICES).expect("services given up open again");
        assert!(third.stop_and_join(Duration::from_secs(5)));
    }

    #[test]
    fn services_held_by_a_thread_that_does_not_end_are_not_taken() {
        let (release, released) = channel::<()>();
        let mut stuck = MoqNodeThread::spawn_holding(
            "test/stuck-holder",
            "stuck",
            Duration::ZERO,
            || Ok(()),
            move |(), _| {
                let _ = released.recv();
            },
        )
        .unwrap();
        let opened_again = AtomicUsize::new(0);
        let refused = MoqNodeThread::spawn_holding(
            "test/stuck-holder",
            "taker",
            Duration::from_millis(20),
            || {
                opened_again.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |(), _| {},
        );
        assert!(refused.is_err());
        assert_eq!(opened_again.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        assert!(stuck.stop_and_join(Duration::from_secs(5)));
    }
}
