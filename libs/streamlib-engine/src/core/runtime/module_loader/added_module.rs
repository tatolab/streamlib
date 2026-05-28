// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The eager async module-load handle.
//!
//! [`Runner::add_module`] / [`Runner::add_module_with`] return an
//! [`AddedModule`] whose work is **already spawned** on the runtime's
//! tokio handle — so issuing N loads kicks off up to N concurrent
//! resolutions/builds before the caller awaits anything. [`AddedModule`]
//! implements [`Future`] directly, so it is awaitable, collectible into
//! a `FuturesUnordered`, and usable anywhere `IntoFuture` is expected.
//! A cache-only load resolves almost immediately; a build-requiring load
//! streams [`ModuleLoadEvent`]s as it progresses.
//!
//! [`Runner::add_module`]: super::super::Runner::add_module
//! [`Runner::add_module_with`]: super::super::Runner::add_module_with

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::errors::AddModuleError;

/// Capacity of each [`AddedModule`]'s progress broadcast channel. Build
/// logs can be chatty; a lagging observer drops the oldest events
/// (`RecvError::Lagged`) without affecting the terminal result, which
/// comes from awaiting the future, never from the event stream.
pub(super) const MODULE_EVENT_CHANNEL_CAPACITY: usize = 256;

/// A module that finished loading. Returned by awaiting an
/// [`AddedModule`].
#[derive(Debug, Clone)]
pub struct LoadedModule {
    /// The module that was loaded.
    pub ident: streamlib_idents::ModuleIdent,
}

/// A progress / diagnostic event emitted while a module loads. Consumers
/// match the variants they care about and ignore the rest (the enum is
/// `#[non_exhaustive]`, so new stages are not breaking).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ModuleLoadEvent {
    /// The load began.
    Started {
        ident: streamlib_idents::ModuleIdent,
    },
    /// Resolved from cache / a ready directory — no build needed.
    CacheHit {
        ident: streamlib_idents::ModuleIdent,
    },
    /// A (re)build is running. `language` is `"rust"` / `"python"` /
    /// `"deno"`.
    Building {
        ident: streamlib_idents::ModuleIdent,
        language: &'static str,
    },
    /// One line of build-tool output.
    BuildLog {
        ident: streamlib_idents::ModuleIdent,
        line: String,
    },
    /// The load completed successfully. `took` is monotonic elapsed.
    Completed {
        ident: streamlib_idents::ModuleIdent,
        took: std::time::Duration,
    },
    /// The load failed. `error` is the rendered [`AddModuleError`].
    Failed {
        ident: streamlib_idents::ModuleIdent,
        error: String,
    },
}

impl ModuleLoadEvent {
    /// The module this event concerns.
    pub fn ident(&self) -> &streamlib_idents::ModuleIdent {
        match self {
            ModuleLoadEvent::Started { ident }
            | ModuleLoadEvent::CacheHit { ident }
            | ModuleLoadEvent::Building { ident, .. }
            | ModuleLoadEvent::BuildLog { ident, .. }
            | ModuleLoadEvent::Completed { ident, .. }
            | ModuleLoadEvent::Failed { ident, .. } => ident,
        }
    }
}

/// Handle to an in-flight (or already-complete) module load.
///
/// The load runs on the runtime's tokio handle from the moment
/// `add_module*` returns. Awaiting drives it to its terminal result;
/// dropping without awaiting **cancels** it (aborts the task and, via
/// the orchestrator, kills any in-flight child build). `#[must_use]`
/// catches the accidental fire-and-forget where a caller drops the
/// handle and never observes the load.
#[must_use = "an AddedModule cancels on drop — await it (or drive it via await_modules) to load the module"]
pub struct AddedModule {
    ident: streamlib_idents::ModuleIdent,
    /// `None` after the future resolves (so `Drop` doesn't abort a
    /// finished task).
    join: Option<tokio::task::JoinHandle<Result<LoadedModule, AddModuleError>>>,
    events: tokio::sync::broadcast::Sender<ModuleLoadEvent>,
    /// The receiver created at construction — i.e. BEFORE the eager load
    /// task starts emitting — so a driver ([`Runner::await_modules`]) that
    /// takes it cannot miss early events despite broadcast's
    /// late-subscriber semantics.
    ///
    /// [`Runner::await_modules`]: super::super::Runner::await_modules
    initial_rx: Option<tokio::sync::broadcast::Receiver<ModuleLoadEvent>>,
}

impl AddedModule {
    /// Construct from a spawned load task, its progress sender, and the
    /// receiver subscribed before the task started. Internal to the
    /// loader.
    pub(super) fn new(
        ident: streamlib_idents::ModuleIdent,
        join: tokio::task::JoinHandle<Result<LoadedModule, AddModuleError>>,
        events: tokio::sync::broadcast::Sender<ModuleLoadEvent>,
        initial_rx: tokio::sync::broadcast::Receiver<ModuleLoadEvent>,
    ) -> Self {
        Self {
            ident,
            join: Some(join),
            events,
            initial_rx: Some(initial_rx),
        }
    }

    /// The module this load resolves.
    pub fn ident(&self) -> &streamlib_idents::ModuleIdent {
        &self.ident
    }

    /// Subscribe to this load's progress events. Late subscribers miss
    /// already-emitted events; the terminal result comes from awaiting
    /// the handle, not from this stream.
    pub fn progress(&self) -> tokio::sync::broadcast::Receiver<ModuleLoadEvent> {
        self.events.subscribe()
    }

    /// Take the construction-time event receiver (catches every event
    /// from `Started` onward). Used by [`Runner::await_modules`] to
    /// forward per-module progress without races. Returns `None` if
    /// already taken.
    ///
    /// [`Runner::await_modules`]: super::super::Runner::await_modules
    pub(super) fn take_event_receiver(
        &mut self,
    ) -> Option<tokio::sync::broadcast::Receiver<ModuleLoadEvent>> {
        self.initial_rx.take()
    }

    /// Cancel the in-flight load. Aborts the task; the orchestrator's
    /// child build (if any) is killed as its process handle drops.
    pub fn cancel(mut self) {
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

impl Future for AddedModule {
    type Output = Result<LoadedModule, AddModuleError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let ident = self.ident.clone();
        let Some(join) = self.join.as_mut() else {
            // Polled after completion — the task was already taken.
            return Poll::Ready(Err(AddModuleError::LoadTaskPanicked {
                module: ident,
                detail: "polled after completion".into(),
            }));
        };
        match Pin::new(join).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(joined) => {
                // Mark terminal so Drop doesn't abort a finished task.
                self.join = None;
                Poll::Ready(match joined {
                    Ok(result) => result,
                    Err(join_err) if join_err.is_cancelled() => {
                        Err(AddModuleError::LoadCancelled { module: ident })
                    }
                    Err(join_err) => Err(AddModuleError::LoadTaskPanicked {
                        module: ident,
                        detail: join_err.to_string(),
                    }),
                })
            }
        }
    }
}

impl Drop for AddedModule {
    fn drop(&mut self) {
        // Drop-without-await is a cancel. A finished task (join already
        // taken in poll) is unaffected.
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}
