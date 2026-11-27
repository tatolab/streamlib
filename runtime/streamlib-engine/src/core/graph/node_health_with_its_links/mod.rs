// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node's health read with the links into it: a link into the node that is not wired, since
//! an end refused it or its two ends no longer carry types that connect, leaves the node reading
//! nothing from that input, so a node its state calls healthy reads not ready, with one
//! diagnostic of that failure's kind placed at each such link.

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticOnTheWire, DiagnosticPlaceOnTheWire, NodeHealthOnTheWire,
};

use super::{Graph, GraphNode};
use crate::core::json_schema::why_the_link_is_not_wired;

impl Graph {
    /// The health of `node` and each diagnostic behind it, read from its state and from each
    /// link into it that could not be wired; `None` for a node that keeps no state.
    pub(crate) fn the_health_and_diagnostics_of(
        &self,
        node: &GraphNode,
    ) -> Option<(NodeHealthOnTheWire, Vec<DiagnosticOnTheWire>)> {
        let (health, mut diagnostics) = node.health_and_diagnostics()?;
        let links_not_wired = self.the_diagnostic_of_each_link_into_the_node_not_wired(node);
        if links_not_wired.is_empty() {
            return Some((health, diagnostics));
        }
        diagnostics.extend(links_not_wired);
        let health = match health {
            NodeHealthOnTheWire::Healthy => NodeHealthOnTheWire::NotReady,
            other => other,
        };
        Some((health, diagnostics))
    }

    /// One diagnostic for each link into `node` that is not wired, of the kind of its failure,
    /// placed at the link and naming why: both pin types of one whose ends no longer connect, the
    /// words of the end that refused any other.
    fn the_diagnostic_of_each_link_into_the_node_not_wired(
        &self,
        node: &GraphNode,
    ) -> Vec<DiagnosticOnTheWire> {
        let name_of = |node_run_id: &super::NodeRunId| {
            self.traversal()
                .v(node_run_id)
                .first()
                .map_or_else(|| node_run_id.to_string(), |held| held.display_name.clone())
        };
        self.traversal()
            .v(&node.id)
            .in_e()
            .iter()
            .filter_map(|link| {
                let why = why_the_link_is_not_wired(link)?;
                Some(DiagnosticOnTheWire::of(
                    why.kind(),
                    DiagnosticPlaceOnTheWire::Link {
                        from_node: name_of(link.source().node_run_id()),
                        from_port: link.source().port_name().to_owned(),
                        to_node: node.display_name.clone(),
                        to_port: link.to_port().port_name().to_owned(),
                    },
                    format!(
                        "its input `{}` reads nothing: the link into it is not wired: {why}. To \
                         fix: {}",
                        link.to_port().port_name(),
                        why.fix()
                    ),
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
