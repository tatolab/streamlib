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
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::{MoqModuleError, Result};

/// Asked once, when the node stops; its thread checks it between passes or
/// waits on it.
#[derive(Default)]
pub struct MoqNodeThreadStopSignal {
    stop_requested: Mutex<bool>,
    stop_requested_changed: Condvar,
    ended: Mutex<bool>,
    ended_changed: Condvar,
}

impl MoqNodeThreadStopSignal {
    /// Whether the node asked its thread to stop.
    pub fn is_requested(&self) -> bool {
        self.stop_requested
            .lock()
            .map(|stop_requested| *stop_requested)
            .unwrap_or(true)
    }

    /// Wait up to `timeout` for a stop; whether one was asked.
    pub fn wait_for_a_stop_up_to(&self, timeout: Duration) -> bool {
        let Ok(stop_requested) = self.stop_requested.lock() else {
            return true;
        };
        self.stop_requested_changed
            .wait_timeout_while(stop_requested, timeout, |stop_requested| !*stop_requested)
            .map(|(stop_requested, _)| *stop_requested)
            .unwrap_or(true)
    }

    fn request(&self) {
        if let Ok(mut stop_requested) = self.stop_requested.lock() {
            *stop_requested = true;
        }
        self.stop_requested_changed.notify_all();
    }

    fn mark_ended(&self) {
        if let Ok(mut ended) = self.ended.lock() {
            *ended = true;
        }
        self.ended_changed.notify_all();
    }

    /// Wait up to `timeout` for the thread to end; whether it did.
    fn wait_until_ended_up_to(&self, timeout: Duration) -> bool {
        let Ok(ended) = self.ended.lock() else {
            return true;
        };
        self.ended_changed
            .wait_timeout_while(ended, timeout, |ended| !*ended)
            .map(|(ended, _)| *ended)
            .unwrap_or(true)
    }
}

/// Marks its thread ended once everything its body held is dropped.
struct MarkedEndedWhenDropped(Arc<MoqNodeThreadStopSignal>);

impl Drop for MarkedEndedWhenDropped {
    fn drop(&mut self) {
        self.0.mark_ended();
    }
}

/// The thread of this process holding each node's services, by the name of the service that
/// names its holder.
static THE_HOLDER_OF_EACH_NODES_SERVICES: LazyLock<
    Mutex<HashMap<String, Arc<MoqNodeThreadStopSignal>>>,
> = LazyLock::new(Mutex::default);

fn the_holders() -> MutexGuard<'static, HashMap<String, Arc<MoqNodeThreadStopSignal>>> {
    THE_HOLDER_OF_EACH_NODES_SERVICES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A node's running thread, stopped and joined by its node.
pub struct MoqNodeThread {
    stop_signal: Arc<MoqNodeThreadStopSignal>,
    holding: Option<String>,
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
        let mut holders = the_holders();
        if let Some(holder_before) = holders.get(holding) {
            holder_before.request();
            if !holder_before.wait_until_ended_up_to(taken_over_within) {
                return Err(MoqModuleError::Ipc {
                    what: format!(
                        "the thread holding `{holding}` before did not end within \
                         {taken_over_within:?}, so its services are not taken from it"
                    ),
                });
            }
            holders.remove(holding);
        }
        let opened = open()?;
        let mut thread = Self::spawn(thread_name, move |stop_signal| body(opened, stop_signal))?;
        holders.insert(holding.to_owned(), Arc::clone(&thread.stop_signal));
        thread.holding = Some(holding.to_owned());
        Ok(thread)
    }

    /// Stop the thread and wait up to `timeout` for it; whether it ended. A thread a later one
    /// took its services from has ended already.
    pub fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.stop_signal.request();
        let Some(thread) = self.thread.take() else {
            return true;
        };
        if !self.stop_signal.wait_until_ended_up_to(timeout) {
            self.thread = Some(thread);
            return false;
        }
        let _ = thread.join();
        if let Some(holding) = self.holding.take() {
            let mut holders = the_holders();
            if holders
                .get(&holding)
                .is_some_and(|holder| Arc::ptr_eq(holder, &self.stop_signal))
            {
                holders.remove(&holding);
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
