// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::core::graph::{LinkDirection, ProcessorUniqueId};

/// Reference to an output port on a processor node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLinkPortRef {
    pub processor_id: ProcessorUniqueId,
    pub port_name: String,
}

impl OutputLinkPortRef {
    /// Direction is always Output for output ports.
    pub const DIRECTION: LinkDirection = LinkDirection::Output;

    pub fn new(processor_id: impl Into<ProcessorUniqueId>, port_name: impl Into<String>) -> Self {
        Self {
            processor_id: processor_id.into(),
            port_name: port_name.into(),
        }
    }

    pub fn direction(&self) -> LinkDirection {
        Self::DIRECTION
    }
}

impl fmt::Display for OutputLinkPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.processor_id, self.port_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// msgpack round-trip preserves both fields. Covers the
    /// runtime-ops-shim encode path the plugin ABI takes when
    /// forwarding `Runtime::connect` calls from cdylib code.
    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        let port_ref =
            OutputLinkPortRef::new(ProcessorUniqueId::from("Pcam"), "video_out");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: OutputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);
    }

    /// Empty port_name round-trips (no field-skipping shenanigans).
    #[test]
    fn msgpack_round_trip_empty_port_name() {
        let port_ref =
            OutputLinkPortRef::new(ProcessorUniqueId::from("P0"), "");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: OutputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);
    }
}
