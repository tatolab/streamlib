// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! An axum router served on an engine's tokio runtime until it is stopped, and
//! the wait for the router — which holds the engine — to be dropped, so the
//! engine is never dropped last on one of its own tokio workers.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// How long letting go of a served router waits for it to be dropped.
const SERVED_ROUTER_DROP_BUDGET: Duration = Duration::from_secs(5);

/// A router being served. Dropping it stops serving and, off a tokio runtime,
/// waits for the router to be dropped.
#[derive(Debug)]
pub(crate) struct AxumRouterServedUntilStopped {
    stopping_token: CancellationToken,
    /// Disconnected once the serving task has dropped the router and all it
    /// holds; never sent on.
    router_dropped_receiver: std::sync::mpsc::Receiver<()>,
    /// What the router serves, as a log line names it.
    served_as: &'static str,
}

impl AxumRouterServedUntilStopped {
    /// Serve `router` on `listener` from `tokio_handle` until `stopping_token`
    /// is cancelled or the returned holder is dropped.
    pub(crate) fn serve<Listener>(
        listener: Listener,
        router: axum::Router,
        stopping_token: CancellationToken,
        served_as: &'static str,
        tokio_handle: &tokio::runtime::Handle,
    ) -> Self
    where
        Listener: axum::serve::Listener,
        Listener::Addr: std::fmt::Debug,
    {
        let (router_dropped_sender, router_dropped_receiver) = std::sync::mpsc::channel::<()>();
        let serving_stopping_token = stopping_token.clone();
        tokio_handle.spawn(async move {
            let served = axum::serve(listener, router)
                .with_graceful_shutdown(serving_stopping_token.cancelled_owned())
                .await;
            if let Err(error) = served {
                tracing::error!(%error, "{served_as} stopped serving");
            }
            drop(router_dropped_sender);
        });
        Self {
            stopping_token,
            router_dropped_receiver,
            served_as,
        }
    }
}

impl Drop for AxumRouterServedUntilStopped {
    fn drop(&mut self) {
        self.stopping_token.cancel();
        // Inside a tokio runtime the serving task may need this very worker to
        // end, so the wait is only for a host letting go from its own thread.
        if tokio::runtime::Handle::try_current().is_ok() {
            return;
        }
        if !matches!(
            self.router_dropped_receiver
                .recv_timeout(SERVED_ROUTER_DROP_BUDGET),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ) {
            tracing::warn!(
                "{}'s router was still held {SERVED_ROUTER_DROP_BUDGET:?} after it stopped \
                 serving; a connection outlived the shutdown",
                self.served_as
            );
        }
    }
}
