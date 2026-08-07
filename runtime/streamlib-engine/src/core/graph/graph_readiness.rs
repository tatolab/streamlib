// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Waiting for a graph's processors to come up.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::components::ObservableProcessorState;
use super::nodes::ProcessorUniqueId;
use crate::core::error::{Error, Result};
use crate::core::processors::ProcessorState;

/// Every processor's state, held directly rather than reached through the
/// graph.
///
/// Taken once and then waited on with no lock held: the transitions being
/// waited for are made by processor threads that need the graph lock to make
/// them, so a waiter holding it would be waiting for itself. The set is fixed
/// at the moment it is taken — a processor added afterwards is not in it.
pub struct ObservableGraphReadiness {
    processor_states: Vec<(ProcessorUniqueId, Arc<ObservableProcessorState>)>,
}

impl ObservableGraphReadiness {
    pub(crate) fn new(
        processor_states: Vec<(ProcessorUniqueId, Arc<ObservableProcessorState>)>,
    ) -> Self {
        Self { processor_states }
    }

    /// Block until every processor has finished `setup` and reached `Running`,
    /// giving up after `timeout`.
    ///
    /// Processors are waited on in turn against one shared deadline, so a
    /// failure is reported when the wait reaches it rather than the instant it
    /// happens. Whichever processor ends the wait, the error names the state
    /// every processor was in, so a failure behind a slow one is still in the
    /// report.
    pub fn wait_until_every_processor_is_running(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;

        for (processor_id, observable_state) in &self.processor_states {
            let settled = observable_state.wait_until_setup_resolved(deadline);
            if settled == ProcessorState::Running {
                continue;
            }
            return Err(Error::Runtime(if settled.is_before_setup_completed() {
                format!(
                    "[{processor_id}] had not finished setup within {timeout:?} (still \
                     {settled}); every processor: {}",
                    self.describe_every_processor()
                )
            } else {
                format!(
                    "[{processor_id}] is {settled} rather than Running; every processor: {}",
                    self.describe_every_processor()
                )
            }));
        }

        Ok(())
    }

    /// Read off the states already held, so the diagnostic never takes the
    /// graph lock — the timeout it explains is most likely a thread wedged
    /// holding exactly that lock.
    fn describe_every_processor(&self) -> String {
        self.processor_states
            .iter()
            .map(|(processor_id, observable_state)| {
                format!("{processor_id}={}", observable_state.current())
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}
