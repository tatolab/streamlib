// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A compile's part in a link an end out of process refused to open: logged again, it is closed
//! the way it was wired and kept in the graph, then wired afresh with the rest of the compile's
//! links, so a far side that is ready now opens it with no stop of the stream.

use super::{CompilationPlan, Compiler};
use crate::core::compiler::PendingOperation;
use crate::core::compiler::compiler_ops::running_node_restart::unwire_each_link_keeping_it;
use crate::core::error::Result;
use crate::core::graph::{Graph, LinkUniqueId, NodeRunId};

/// Close each link `plan` wires again after an end refused it, keeping it in the graph, so the
/// wiring that follows opens it afresh. One `plan` no longer adds, one it holds unwired for its
/// pin types, and one `links_a_restart_unwires` closes already are left to what does.
pub(super) fn unwire_each_link_an_end_refused_keeping_it(
    graph: &mut Graph,
    plan: &CompilationPlan,
    links_a_restart_unwires: &[LinkUniqueId],
) -> Result<()> {
    let links_to_close: Vec<LinkUniqueId> = plan
        .links_to_wire_again
        .iter()
        .filter(|link_id| {
            plan.links_to_add.contains(link_id)
                && !plan.links_to_hold_unwired.contains(link_id)
                && !links_a_restart_unwires.contains(link_id)
        })
        .cloned()
        .collect();
    unwire_each_link_keeping_it(graph, &links_to_close)
}

impl Compiler {
    /// Log an add of each link into or out of `node_run_id` that an end out of process refused,
    /// as a reconnect of it would, so the next commit wires each again; whether it logged any.
    pub(crate) fn log_each_link_of_a_node_an_end_refused_to_wire_it_again(
        &self,
        node_run_id: &NodeRunId,
    ) -> bool {
        self.scope(|graph, tx| {
            let refused = graph.the_links_of_a_node_an_end_refused(node_run_id);
            for link_id in &refused {
                tx.log(PendingOperation::AddLink(link_id.clone()));
            }
            !refused.is_empty()
        })
    }
}
