// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::core::error::Result;
use crate::core::graph::{LinkDirection, NodeRunId, cast_exposed_name_to_url_safe};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLinkPortRef {
    node_run_id: NodeRunId,
    port_name: String,
}

impl OutputLinkPortRef {
    /// Direction is always Output for output ports.
    pub const DIRECTION: LinkDirection = LinkDirection::Output;

    /// A port on this runtime.
    pub fn new(node_run_id: impl Into<NodeRunId>, port_name: impl Into<String>) -> Self {
        Self {
            node_run_id: node_run_id.into(),
            port_name: port_name.into(),
        }
    }

    pub fn direction(&self) -> LinkDirection {
        Self::DIRECTION
    }

    pub fn node_run_id(&self) -> &NodeRunId {
        &self.node_run_id
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// This reference with its port name cast, the way every port is named.
    pub fn with_its_port_name_cast(self) -> Result<Self> {
        Ok(Self {
            port_name: cast_exposed_name_to_url_safe(&self.port_name)?.into_owned(),
            node_run_id: self.node_run_id,
        })
    }
}

impl fmt::Display for OutputLinkPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node_run_id, self.port_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// msgpack round-trip preserves both fields as the map a port reference
    /// has always ridden.
    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        let port_ref = OutputLinkPortRef::new(NodeRunId::from("Pcam"), "video_out");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: OutputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);

        let as_a_map: serde_json::Value = rmp_serde::from_slice(&bytes).expect("a msgpack map");
        assert_eq!(
            as_a_map,
            serde_json::json!({ "node_run_id": "Pcam", "port_name": "video_out" })
        );
    }

    /// Empty port_name round-trips (no field-skipping shenanigans).
    #[test]
    fn msgpack_round_trip_empty_port_name() {
        let port_ref = OutputLinkPortRef::new(NodeRunId::from("P0"), "");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: OutputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);
    }

    #[test]
    fn a_reference_renders_as_its_node_and_port() {
        assert_eq!(
            OutputLinkPortRef::new(NodeRunId::from("Pcam"), "video").to_string(),
            "Pcam.video"
        );
    }
}
