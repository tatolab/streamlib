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
    /// The member of a bundle port the link starts at; `None` for the whole port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    member: Option<String>,
}

impl OutputLinkPortRef {
    /// Direction is always Output for output ports.
    pub const DIRECTION: LinkDirection = LinkDirection::Output;

    /// A port on this runtime.
    pub fn new(node_run_id: impl Into<NodeRunId>, port_name: impl Into<String>) -> Self {
        Self {
            node_run_id: node_run_id.into(),
            port_name: port_name.into(),
            member: None,
        }
    }

    /// This port's member `member`, which a link starting there carries alone; `None` names the
    /// whole port.
    pub fn at_member(self, member: Option<String>) -> Self {
        Self { member, ..self }
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

    /// The member of a bundle port the link starts at; `None` for the whole port.
    pub fn member(&self) -> Option<&str> {
        self.member.as_deref()
    }

    /// This reference with its port name cast, the way every port is named.
    pub fn with_its_port_name_cast(self) -> Result<Self> {
        Ok(Self {
            port_name: cast_exposed_name_to_url_safe(&self.port_name)?.into_owned(),
            node_run_id: self.node_run_id,
            member: self.member,
        })
    }
}

impl fmt::Display for OutputLinkPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node_run_id, self.port_name)?;
        match &self.member {
            Some(member) => write!(f, ".{member}"),
            None => Ok(()),
        }
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

    /// A reference to one member rides the map with its member, and renders after its port.
    #[test]
    fn a_reference_to_a_member_round_trips_and_renders_after_its_port() {
        let port_ref = OutputLinkPortRef::new(NodeRunId::from("Pcam"), "placement")
            .at_member(Some("opacity".to_owned()));
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: OutputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);
        assert_eq!(back.member(), Some("opacity"));
        assert_eq!(port_ref.to_string(), "Pcam.placement.opacity");
    }

    #[test]
    fn a_reference_renders_as_its_node_and_port() {
        assert_eq!(
            OutputLinkPortRef::new(NodeRunId::from("Pcam"), "video").to_string(),
            "Pcam.video"
        );
    }
}
