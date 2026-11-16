// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::components::ObservableNodeState;
use super::nodes::NodeRunId;
use crate::core::error::{Error, Result};
use crate::core::nodes::NodeState;

pub struct ObservableGraphReadiness {
    node_states: Vec<(NodeRunId, Arc<ObservableNodeState>)>,
}

impl ObservableGraphReadiness {
    pub(crate) fn new(node_states: Vec<(NodeRunId, Arc<ObservableNodeState>)>) -> Self {
        Self { node_states }
    }

    pub fn wait_until_every_node_is_running(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;

        for (node_run_id, observable_state) in &self.node_states {
            let settled = observable_state.wait_until_setup_resolved(deadline);
            if settled == NodeState::Running {
                continue;
            }
            return Err(Error::Runtime(if settled.is_before_setup_completed() {
                format!(
                    "[{node_run_id}] had not finished setup within {timeout:?} (still \
                     {settled}); every node: {}",
                    self.describe_every_node()
                )
            } else {
                let reason = observable_state
                    .failure_reason()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default();
                format!(
                    "[{node_run_id}] is {settled} rather than Running{reason}; every \
                     node: {}",
                    self.describe_every_node()
                )
            }));
        }

        Ok(())
    }

    pub fn wait_until_every_node_has_finished_setup(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        self.node_states.iter().all(|(_, observable_state)| {
            let settled = observable_state.wait_until_setup_resolved(deadline);
            !settled.is_before_setup_completed() && settled != NodeState::Restarting
        })
    }

    /// Read off the states already held, so the diagnostic never takes the
    /// graph lock — the timeout it explains is most likely a thread wedged
    /// holding exactly that lock.
    fn describe_every_node(&self) -> String {
        self.node_states
            .iter()
            .map(|(node_run_id, observable_state)| {
                format!("{node_run_id}={}", observable_state.current())
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_nodes_reason_rides_the_readiness_error() {
        let running = Arc::new(ObservableNodeState::new(NodeState::Running));
        let refused = Arc::new(ObservableNodeState::new(NodeState::Pending));
        refused.fail_with(
            "setup failed: VirtualCameraSink \"Desk cam\": no permission to create a \
             v4l2loopback camera",
        );
        let readiness = ObservableGraphReadiness::new(vec![
            (NodeRunId::from("Psource"), running),
            (NodeRunId::from("Psink"), refused),
        ]);

        let reported = readiness
            .wait_until_every_node_is_running(Duration::from_millis(50))
            .expect_err("a refused node is not running")
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
    fn a_paused_node_has_finished_setup_though_it_is_not_running() {
        let paused = Arc::new(ObservableNodeState::new(NodeState::Paused));
        let readiness = ObservableGraphReadiness::new(vec![
            (
                NodeRunId::from("Psource"),
                Arc::new(ObservableNodeState::new(NodeState::Running)),
            ),
            (NodeRunId::from("Pidle"), paused),
        ]);

        assert!(readiness.wait_until_every_node_has_finished_setup(Duration::from_millis(50)));
    }

    #[test]
    fn a_node_still_in_setup_has_not_finished_it() {
        let readiness = ObservableGraphReadiness::new(vec![(
            NodeRunId::from("Pslow"),
            Arc::new(ObservableNodeState::new(NodeState::Pending)),
        )]);

        assert!(!readiness.wait_until_every_node_has_finished_setup(Duration::from_millis(50)));
    }

    #[test]
    fn a_failure_without_a_reason_reports_the_state_alone() {
        let failed = Arc::new(ObservableNodeState::new(NodeState::Pending));
        failed.transition_to(NodeState::Error);
        let readiness = ObservableGraphReadiness::new(vec![(NodeRunId::from("Pquiet"), failed)]);

        let reported = readiness
            .wait_until_every_node_is_running(Duration::from_millis(50))
            .expect_err("failed")
            .to_string();

        assert!(
            reported.contains("[Pquiet] is Error rather than Running; every node"),
            "{reported}"
        );
    }
}
