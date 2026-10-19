// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! An axum router served on an engine's tokio runtime until it is stopped, and
//! the wait for the router — which holds the engine — to be dropped, so the
//! engine is never dropped last on one of its own tokio workers.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// How long letting go of a served router waits for it to be dropped.
const SERVED_ROUTER_DROP_BUDGET: Duration = Duration::from_secs(5);

/// A router being served. Dropping it stops serving without waiting.
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

    /// Stop serving and block for up to `budget` until the router — and all it
    /// holds — has been dropped, answering whether it was. Never call it from
    /// inside the tokio runtime serving the router: that would block the very
    /// worker the serving task ends on.
    fn stop_serving_and_wait_until_the_router_is_dropped(self, budget: Duration) -> bool {
        self.stopping_token.cancel();
        matches!(
            self.router_dropped_receiver.recv_timeout(budget),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        )
    }

    /// Stop serving and, when let go from outside a tokio runtime, wait for the
    /// router to be dropped, warning when a connection outlives the wait.
    /// Inside a tokio runtime the serving task may need this very worker to
    /// end, so there is no wait.
    pub(crate) fn stop_serving_and_let_go_of_the_router(self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            return;
        }
        let served_as = self.served_as;
        if !self.stop_serving_and_wait_until_the_router_is_dropped(SERVED_ROUTER_DROP_BUDGET) {
            tracing::warn!(
                "{served_as}'s router was still held {SERVED_ROUTER_DROP_BUDGET:?} after it \
                 stopped serving; a connection outlived the shutdown"
            );
        }
    }
}

impl Drop for AxumRouterServedUntilStopped {
    fn drop(&mut self) {
        self.stopping_token.cancel();
    }
}
