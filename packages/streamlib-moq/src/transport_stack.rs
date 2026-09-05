// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What this wheel's `load(host)` hook brings up, once per process.
//!
//! Both of these are process-global by construction — rustls keeps one default
//! crypto provider, and one tokio runtime is what the sessions in this process
//! share — which is why they belong in the support hook rather than in a
//! processor's `setup()`, where every instance would race to install them.

use crate::error::{MoqExtensionError, Result};
use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// The build's own result, not just the runtime: `get_or_init` runs its closure
/// at most once and cannot fail, so the failure has to be what is stored. The
/// alternative — build outside and store the winner — drops the loser's
/// runtime, and dropping a tokio runtime from inside an async context panics.
static TRANSPORT_RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();

/// A MoQ session keeps more tasks alive than a peer connection does — the
/// session's own control loop, the namespace publish loop, and one drain per
/// subscribed track — so it gets a worker more than a single-threaded runtime
/// would give it. They are tasks, not threads: this is the pool they share.
const TRANSPORT_RUNTIME_WORKER_THREADS: usize = 2;

/// Install the TLS provider and start the runtime. Cheap, and does no I/O:
/// `Runtime()` is waiting on this in the app process, and a helper is inside
/// its registration budget.
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
                .thread_name("streamlib-moq")
                .enable_all()
                .build()
                .map_err(|failure| failure.to_string())
        })
        .as_ref()
        .map_err(|failure| MoqExtensionError::Transport {
            what: format!("the MoQ transport runtime could not be started: {failure}"),
        })
}
