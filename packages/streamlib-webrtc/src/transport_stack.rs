// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The TLS provider and the tokio runtime this wheel's sessions share, brought
//! up on first use in whichever process the sessions run in.
//!
//! Both are process-global by construction — rustls keeps one default crypto
//! provider, and one tokio runtime is what the sessions in a process share — so
//! the first session to need them brings them up and every later call is a
//! no-op, rather than each processor instance racing to install them.

use crate::error::{Result, WebRtcExtensionError};
use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// The build's own result, not just the runtime: `get_or_init` runs its closure
/// at most once and cannot fail, so the failure has to be what is stored. The
/// alternative — build outside and store the winner — drops the loser's
/// runtime, and dropping a tokio runtime from inside an async context panics.
static TRANSPORT_RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();

/// Two threads is what a session needs: one driving the peer connection's
/// timers and sockets, one for the track read and write loops.
const TRANSPORT_RUNTIME_WORKER_THREADS: usize = 2;

/// Install the TLS provider and start the runtime, once per process. Cheap,
/// and does no I/O.
pub(crate) fn bring_up() -> Result<()> {
    // Already installed is the ordinary case — another extension in this
    // process may have got there first, and the provider is shared.
    if rustls::crypto::CryptoProvider::get_default().is_none()
        && rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
    {
        tracing::debug!("another caller installed the rustls crypto provider first");
    }

    transport_runtime().map(|_| ())
}

/// The runtime every session in this process runs on.
pub(crate) fn transport_runtime() -> Result<&'static Runtime> {
    TRANSPORT_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(TRANSPORT_RUNTIME_WORKER_THREADS)
                .thread_name("streamlib-webrtc")
                .enable_all()
                .build()
                .map_err(|failure| failure.to_string())
        })
        .as_ref()
        .map_err(|failure| WebRtcExtensionError::Transport {
            what: format!("the WebRTC transport runtime could not be started: {failure}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bringing_the_stack_up_twice_installs_a_provider_and_keeps_one_runtime() {
        bring_up().expect("the first bring-up succeeds");
        let first_runtime: *const Runtime = transport_runtime().expect("the runtime is up");

        bring_up().expect("a second bring-up is not an error");
        let second_runtime: *const Runtime = transport_runtime().expect("the runtime is up");

        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
        assert!(std::ptr::eq(first_runtime, second_runtime));
    }
}
