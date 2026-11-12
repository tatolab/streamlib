// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::compiler::Compiler;
use crate::core::context::RuntimeContext;
use crate::core::error::Result;
use crate::core::logging::polled_in_a_project_log_route_entry;
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

impl GraphChangeListener {
    /// Compile the stream's graph on the async runtime, once the stream is started.
    fn commit_the_graph_once_started(&self) {
        if *self.status.lock() != RuntimeStatus::Started {
            tracing::debug!(
                "[GraphChangeListener] Runtime not started, commit deferred to start()"
            );
            return;
        }
        let Some(runtime_context) = self.runtime_context.lock().clone() else {
            tracing::debug!("[GraphChangeListener] No runtime context, skipping commit");
            return;
        };
        let compiler = Arc::clone(&self.compiler);
        let runtime_context_for_the_commit = Arc::clone(&runtime_context);
        runtime_context
            .tokio_handle()
            .spawn(polled_in_a_project_log_route_entry(
                Arc::clone(runtime_context.this_streams_log_route()),
                async move {
                    if let Err(e) = compiler.commit(&runtime_context_for_the_commit) {
                        tracing::error!("[GraphChangeListener] Commit failed: {}", e);
                    }
                },
            ));
    }
}

impl EventListener for GraphChangeListener {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        if let Event::OfALoadedStream {
            event: RuntimeEvent::GraphDidChange,
            ..
        } = event
        {
            self.commit_the_graph_once_started();
        }
        Ok(())
    }

    /// A lost event may have been a `GraphDidChange`, so the graph is compiled as if it was.
    fn on_events_lost(&mut self, _lost_event_count: u64) -> Result<()> {
        self.commit_the_graph_once_started();
        Ok(())
    }
}
