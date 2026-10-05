// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::core::error::Result;
use crate::core::graph::{LinkDirection, ProcessorUniqueId, cast_exposed_name_to_url_safe};

/// Reference to the input port a link carries into, on a processor this
/// runtime's own graph holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputLinkPortRef {
    processor_id: ProcessorUniqueId,
    port_name: String,
}

impl InputLinkPortRef {
    /// Direction is always Input for input ports.
    pub const DIRECTION: LinkDirection = LinkDirection::Input;

    /// A port on this runtime.
    pub fn new(processor_id: impl Into<ProcessorUniqueId>, port_name: impl Into<String>) -> Self {
        Self {
            processor_id: processor_id.into(),
            port_name: port_name.into(),
        }
    }

    pub fn direction(&self) -> LinkDirection {
        Self::DIRECTION
    }

    /// The processor the port belongs to.
    pub fn processor_id(&self) -> &ProcessorUniqueId {
        &self.processor_id
    }

    /// The port's own name on that processor.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// This reference with its port name cast, the way every port is named.
    pub fn with_its_port_name_cast(self) -> Result<Self> {
        Ok(Self {
            port_name: cast_exposed_name_to_url_safe(&self.port_name)?.into_owned(),
            processor_id: self.processor_id,
        })
    }
}

impl fmt::Display for InputLinkPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.processor_id, self.port_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// msgpack round-trip preserves both fields as the map a port reference
    /// has always ridden.
    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        let port_ref = InputLinkPortRef::new(ProcessorUniqueId::from("Pdisplay"), "video_in");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: InputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);

        let as_a_map: serde_json::Value = rmp_serde::from_slice(&bytes).expect("a msgpack map");
        assert_eq!(
            as_a_map,
            serde_json::json!({ "processor_id": "Pdisplay", "port_name": "video_in" })
        );
    }

    #[test]
    fn msgpack_round_trip_empty_port_name() {
        let port_ref = InputLinkPortRef::new(ProcessorUniqueId::from("P0"), "");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: InputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);
    }

    /// A reference renders as its processor and port.
    #[test]
    fn a_reference_renders_as_its_processor_and_port() {
        assert_eq!(
            InputLinkPortRef::new(ProcessorUniqueId::from("Pdisplay"), "video").to_string(),
            "Pdisplay.video"
        );
    }
}
