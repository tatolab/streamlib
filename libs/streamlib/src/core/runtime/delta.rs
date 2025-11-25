//! Delta application for hot graph updates
//!
//! This module provides the ability to compute and apply deltas between
//! execution plans, enabling hot reloading of graph changes without
//! requiring a full restart.
//!
//! # Overview
//!
//! When the graph changes while the runtime is Running or Paused, we need to:
//! 1. Compute what changed (added/removed processors and connections)
//! 2. Apply those changes incrementally without disrupting existing processors
//!
//! # Phase 1 Implementation
//!
//! For Phase 1, delta application is limited to:
//! - Adding new processors (spawn new threads)
//! - Removing processors (shutdown and join threads)
//! - Adding new connections (wire new connections)
//! - Removing connections (unwire existing connections)
//!
//! More advanced features like processor migration and buffer resizing
//! will be added in future phases.

use std::collections::HashSet;

use crate::core::bus::ConnectionId;
use crate::core::graph_optimizer::ExecutionPlan;
use crate::core::handles::ProcessorId;

/// Delta between two execution plans
///
/// Represents the changes needed to transform from the old plan to the new plan.
#[derive(Debug, Clone, Default)]
pub struct ExecutionDelta {
    /// Processors to add (new threads to spawn)
    pub processors_to_add: Vec<ProcessorId>,
    /// Processors to remove (threads to shutdown)
    pub processors_to_remove: Vec<ProcessorId>,
    /// Connections to add (wire new connections)
    pub connections_to_add: Vec<ConnectionId>,
    /// Connections to remove (unwire existing connections)
    pub connections_to_remove: Vec<ConnectionId>,
}

impl ExecutionDelta {
    /// Check if delta is empty (no changes)
    pub fn is_empty(&self) -> bool {
        self.processors_to_add.is_empty()
            && self.processors_to_remove.is_empty()
            && self.connections_to_add.is_empty()
            && self.connections_to_remove.is_empty()
    }

    /// Get total number of changes
    pub fn change_count(&self) -> usize {
        self.processors_to_add.len()
            + self.processors_to_remove.len()
            + self.connections_to_add.len()
            + self.connections_to_remove.len()
    }
}

/// Compute delta between two execution plans
///
/// Returns the changes needed to go from `old_plan` to `new_plan`.
pub fn compute_delta(old_plan: &ExecutionPlan, new_plan: &ExecutionPlan) -> ExecutionDelta {
    match (old_plan, new_plan) {
        (
            ExecutionPlan::Legacy {
                processors: old_procs,
                connections: old_conns,
            },
            ExecutionPlan::Legacy {
                processors: new_procs,
                connections: new_conns,
            },
        ) => {
            let old_proc_set: HashSet<_> = old_procs.iter().collect();
            let new_proc_set: HashSet<_> = new_procs.iter().collect();

            let old_conn_set: HashSet<_> = old_conns.iter().collect();
            let new_conn_set: HashSet<_> = new_conns.iter().collect();

            ExecutionDelta {
                // Processors in new but not in old = to add
                processors_to_add: new_procs
                    .iter()
                    .filter(|p| !old_proc_set.contains(p))
                    .cloned()
                    .collect(),
                // Processors in old but not in new = to remove
                processors_to_remove: old_procs
                    .iter()
                    .filter(|p| !new_proc_set.contains(p))
                    .cloned()
                    .collect(),
                // Connections in new but not in old = to add
                connections_to_add: new_conns
                    .iter()
                    .filter(|c| !old_conn_set.contains(c))
                    .cloned()
                    .collect(),
                // Connections in old but not in new = to remove
                connections_to_remove: old_conns
                    .iter()
                    .filter(|c| !new_conn_set.contains(c))
                    .cloned()
                    .collect(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bus::connection_id::__private::new_unchecked;

    fn make_legacy_plan(processors: Vec<&str>, connections: Vec<&str>) -> ExecutionPlan {
        ExecutionPlan::Legacy {
            processors: processors.into_iter().map(String::from).collect(),
            connections: connections.into_iter().map(|s| new_unchecked(s)).collect(),
        }
    }

    #[test]
    fn test_empty_delta_for_identical_plans() {
        let plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec!["conn_0"]);

        let delta = compute_delta(&plan, &plan);

        assert!(delta.is_empty());
        assert_eq!(delta.change_count(), 0);
    }

    #[test]
    fn test_add_processor() {
        let old_plan = make_legacy_plan(vec!["proc_0"], vec![]);
        let new_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec![]);

        let delta = compute_delta(&old_plan, &new_plan);

        assert_eq!(delta.processors_to_add.len(), 1);
        assert_eq!(delta.processors_to_add[0], "proc_1");
        assert!(delta.processors_to_remove.is_empty());
        assert_eq!(delta.change_count(), 1);
    }

    #[test]
    fn test_remove_processor() {
        let old_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec![]);
        let new_plan = make_legacy_plan(vec!["proc_0"], vec![]);

        let delta = compute_delta(&old_plan, &new_plan);

        assert!(delta.processors_to_add.is_empty());
        assert_eq!(delta.processors_to_remove.len(), 1);
        assert_eq!(delta.processors_to_remove[0], "proc_1");
    }

    #[test]
    fn test_add_connection() {
        let old_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec![]);
        let new_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec!["conn_0"]);

        let delta = compute_delta(&old_plan, &new_plan);

        assert_eq!(delta.connections_to_add.len(), 1);
        assert_eq!(&*delta.connections_to_add[0], "conn_0");
        assert!(delta.connections_to_remove.is_empty());
    }

    #[test]
    fn test_remove_connection() {
        let old_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec!["conn_0"]);
        let new_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec![]);

        let delta = compute_delta(&old_plan, &new_plan);

        assert!(delta.connections_to_add.is_empty());
        assert_eq!(delta.connections_to_remove.len(), 1);
        assert_eq!(&*delta.connections_to_remove[0], "conn_0");
    }

    #[test]
    fn test_complex_delta() {
        let old_plan =
            make_legacy_plan(vec!["proc_0", "proc_1", "proc_2"], vec!["conn_0", "conn_1"]);
        let new_plan =
            make_legacy_plan(vec!["proc_0", "proc_3", "proc_4"], vec!["conn_1", "conn_2"]);

        let delta = compute_delta(&old_plan, &new_plan);

        // proc_1 and proc_2 removed, proc_3 and proc_4 added
        assert_eq!(delta.processors_to_add.len(), 2);
        assert!(delta.processors_to_add.contains(&"proc_3".to_string()));
        assert!(delta.processors_to_add.contains(&"proc_4".to_string()));

        assert_eq!(delta.processors_to_remove.len(), 2);
        assert!(delta.processors_to_remove.contains(&"proc_1".to_string()));
        assert!(delta.processors_to_remove.contains(&"proc_2".to_string()));

        // conn_0 removed, conn_2 added
        assert_eq!(delta.connections_to_add.len(), 1);
        assert_eq!(&*delta.connections_to_add[0], "conn_2");

        assert_eq!(delta.connections_to_remove.len(), 1);
        assert_eq!(&*delta.connections_to_remove[0], "conn_0");

        assert_eq!(delta.change_count(), 6);
    }

    #[test]
    fn test_empty_to_populated() {
        let old_plan = make_legacy_plan(vec![], vec![]);
        let new_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec!["conn_0"]);

        let delta = compute_delta(&old_plan, &new_plan);

        assert_eq!(delta.processors_to_add.len(), 2);
        assert_eq!(delta.connections_to_add.len(), 1);
        assert!(delta.processors_to_remove.is_empty());
        assert!(delta.connections_to_remove.is_empty());
    }

    #[test]
    fn test_populated_to_empty() {
        let old_plan = make_legacy_plan(vec!["proc_0", "proc_1"], vec!["conn_0"]);
        let new_plan = make_legacy_plan(vec![], vec![]);

        let delta = compute_delta(&old_plan, &new_plan);

        assert!(delta.processors_to_add.is_empty());
        assert!(delta.connections_to_add.is_empty());
        assert_eq!(delta.processors_to_remove.len(), 2);
        assert_eq!(delta.connections_to_remove.len(), 1);
    }
}
