// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::compiler::Compiler;
use crate::core::context::RuntimeContext;
use crate::core::error::Result;
use crate::core::logging::polled_in_a_loaded_stream_log_route;
use crate::core::pubsub::{Event, EventListener, RuntimeEvent};

use super::RuntimeStatus;

/// Listener, subscribed to one stream's topic, that compiles that stream's
/// graph when it changes.
pub(crate) struct GraphChangeListener {
    status: Arc<Mutex<RuntimeStatus>>,
    runtime_context: Arc<Mutex<Option<Arc<RuntimeContext>>>>,
    compiler: Arc<Compiler>,
}

impl GraphChangeListener {
    pub fn new(
        status: Arc<Mutex<RuntimeStatus>>,
        runtime_context: Arc<Mutex<Option<Arc<RuntimeContext>>>>,
        compiler: Arc<Compiler>,
    ) -> Self {
        Self {
            status,
            runtime_context,
            compiler,
        }
    }
}

impl EventListener for GraphChangeListener {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        if let Event::OfALoadedStream {
            event: RuntimeEvent::GraphDidChange,
            ..
        } = event
        {
            // Only compile when runtime is started
            if *self.status.lock() != RuntimeStatus::Started {
                tracing::debug!(
                    "[GraphChangeListener] Runtime not started, commit deferred to start()"
                );
                return Ok(());
            }

            // Get runtime context
            let ctx = match self.runtime_context.lock().clone() {
                Some(ctx) => ctx,
                None => {
                    tracing::debug!("[GraphChangeListener] No runtime context, skipping commit");
                    return Ok(());
                }
            };

            // Dispatch commit to tokio (non-blocking)
            // Note: compiler.commit() has no main thread requirements - uses only thread-safe primitives
            let compiler = Arc::clone(&self.compiler);
            let ctx_for_closure = Arc::clone(&ctx);
            ctx.tokio_handle()
                .spawn(polled_in_a_loaded_stream_log_route(
                    Arc::clone(ctx.this_streams_log_route()),
                    async move {
                        if let Err(e) = compiler.commit(&ctx_for_closure) {
                            tracing::error!("[GraphChangeListener] Commit failed: {}", e);
                        }
                    },
                ));
        }
        Ok(())
    }
}
