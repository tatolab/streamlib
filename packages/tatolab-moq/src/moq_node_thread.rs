// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The thread each MoQ node's native half runs on, and the signal that stops it.

use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::{MoqModuleError, Result};

/// Asked once, when the node stops; its thread checks it between passes or
/// waits on it.
#[derive(Default)]
pub struct MoqNodeThreadStopSignal {
    stop_requested: Mutex<bool>,
    stop_requested_changed: Condvar,
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
}

/// A node's running thread, stopped and joined by its node.
pub struct MoqNodeThread {
    stop_signal: Arc<MoqNodeThreadStopSignal>,
    ended: Mutex<Receiver<()>>,
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
        let stop_signal_on_the_thread = Arc::clone(&stop_signal);
        let (ended_when_dropped, ended) = channel::<()>();
        let thread = std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let _ended_when_dropped = ended_when_dropped;
                body(&stop_signal_on_the_thread);
            })
            .map_err(|refusal| MoqModuleError::Relay {
                what: format!("the `{thread_name}` thread did not start: {refusal}"),
            })?;
        Ok(Self {
            stop_signal,
            ended: Mutex::new(ended),
            thread: Some(thread),
        })
    }

    /// Stop the thread and wait up to `timeout` for it; whether it ended.
    pub fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.stop_signal.request();
        let Some(thread) = self.thread.take() else {
            return true;
        };
        let ended = match self.ended.get_mut() {
            Ok(ended) => ended.recv_timeout(timeout),
            Err(_) => Err(RecvTimeoutError::Disconnected),
        };
        match ended {
            Err(RecvTimeoutError::Timeout) => {
                self.thread = Some(thread);
                false
            }
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                true
            }
        }
    }
}

impl Drop for MoqNodeThread {
    fn drop(&mut self) {
        self.stop_and_join(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
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
}
