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
    /// report — and a processor whose `setup()` refused carries that refusal's
    /// own text, so the caller reads why rather than only that.
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
                let reason = observable_state
                    .failure_reason()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default();
                format!(
                    "[{processor_id}] is {settled} rather than Running{reason}; every \
                     processor: {}",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The caller who asked whether the graph is up is told, in the refusing
    /// processor's own words, why it is not — the state alone would send them
    /// to the logs for what the error already knows.
    #[test]
    fn a_failed_processors_reason_rides_the_readiness_error() {
        let running = Arc::new(ObservableProcessorState::new(ProcessorState::Running));
        let refused = Arc::new(ObservableProcessorState::new(ProcessorState::Pending));
        refused.fail_with(
            "setup failed: VirtualCameraSink \"Desk cam\": no permission to create a \
             v4l2loopback camera",
        );
        let readiness = ObservableGraphReadiness::new(vec![
            (ProcessorUniqueId::from("Psource"), running),
            (ProcessorUniqueId::from("Psink"), refused),
        ]);

        let reported = readiness
            .wait_until_every_processor_is_running(Duration::from_millis(50))
            .expect_err("a refused processor is not running")
            .to_string();

        assert!(
            reported.contains(
                "[Psink] is Error rather than Running: setup failed: \
            VirtualCameraSink \"Desk cam\": no permission to create a v4l2loopback camera"
            ),
            "{reported}"
        );
        assert!(reported.contains("Psource=Running"), "{reported}");
        assert!(reported.contains("Psink=Error"), "{reported}");
    }

    /// A processor that failed without a reason still reads as before: the
    /// state, and nothing invented after it.
    #[test]
    fn a_failure_without_a_reason_reports_the_state_alone() {
        let failed = Arc::new(ObservableProcessorState::new(ProcessorState::Pending));
        failed.transition_to(ProcessorState::Error);
        let readiness =
            ObservableGraphReadiness::new(vec![(ProcessorUniqueId::from("Pquiet"), failed)]);

        let reported = readiness
            .wait_until_every_processor_is_running(Duration::from_millis(50))
            .expect_err("failed")
            .to_string();

        assert!(
            reported.contains("[Pquiet] is Error rather than Running; every processor"),
            "{reported}"
        );
    }
}
