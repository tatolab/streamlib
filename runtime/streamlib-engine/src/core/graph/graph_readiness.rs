// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::sync::Arc;
use std::time::{Duration, Instant};

use super::components::ObservableProcessorState;
use super::nodes::ProcessorUniqueId;
use crate::core::error::{Error, Result};
use crate::core::processors::ProcessorState;

pub struct ObservableGraphReadiness {
    processor_states: Vec<(ProcessorUniqueId, Arc<ObservableProcessorState>)>,
}

impl ObservableGraphReadiness {
    pub(crate) fn new(
        processor_states: Vec<(ProcessorUniqueId, Arc<ObservableProcessorState>)>,
    ) -> Self {
        Self { processor_states }
    }

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

    pub fn wait_until_every_processor_has_finished_setup(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        self.processor_states.iter().all(|(_, observable_state)| {
            let settled = observable_state.wait_until_setup_resolved(deadline);
            !settled.is_before_setup_completed() && settled != ProcessorState::Restarting
        })
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

    #[test]
    fn a_paused_processor_has_finished_setup_though_it_is_not_running() {
        let paused = Arc::new(ObservableProcessorState::new(ProcessorState::Paused));
        let readiness = ObservableGraphReadiness::new(vec![
            (
                ProcessorUniqueId::from("Psource"),
                Arc::new(ObservableProcessorState::new(ProcessorState::Running)),
            ),
            (ProcessorUniqueId::from("Pidle"), paused),
        ]);

        assert!(readiness.wait_until_every_processor_has_finished_setup(Duration::from_millis(50)));
    }

    #[test]
    fn a_processor_still_in_setup_has_not_finished_it() {
        let readiness = ObservableGraphReadiness::new(vec![(
            ProcessorUniqueId::from("Pslow"),
            Arc::new(ObservableProcessorState::new(ProcessorState::Pending)),
        )]);

        assert!(
            !readiness.wait_until_every_processor_has_finished_setup(Duration::from_millis(50))
        );
    }

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
