// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The ends of a link a connect names, refused when the graph cannot take them.

use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};

/// Refuse `from` when the graph holds no node of its id, or none with an output of its name.
pub(super) fn refuse_a_source_this_graph_cannot_take(
    graph: &crate::core::graph::Graph,
    from: &OutputLinkPortRef,
) -> Result<()> {
    let from_node = graph
        .traversal()
        .v(from.node_run_id())
        .first()
        .ok_or_else(|| Error::NodeNotFound(from.node_run_id().to_string()))?;
    if !from_node.has_output(from.port_name()) {
        return Err(Error::NodePortNotFound {
            node_run_id: from.node_run_id().to_string(),
            port_name: from.port_name().to_string(),
            direction: PortDirection::Output,
        });
    }
    Ok(())
}

/// Refuse `to` when the graph holds no node of its id, or none with an input of its name, or
/// the input is a setting, which takes no link.
pub(super) fn refuse_a_destination_this_graph_cannot_take(
    graph: &crate::core::graph::Graph,
    to: &InputLinkPortRef,
) -> Result<()> {
    let to_node = graph
        .traversal()
        .v(to.node_run_id())
        .first()
        .ok_or_else(|| Error::NodeNotFound(to.node_run_id().to_string()))?;
    if !to_node.has_input(to.port_name()) {
        return Err(Error::NodePortNotFound {
            node_run_id: to.node_run_id().to_string(),
            port_name: to.port_name().to_string(),
            direction: PortDirection::Input,
        });
    }
    to_node.refuse_a_link_into_a_setting(to.port_name())
}
