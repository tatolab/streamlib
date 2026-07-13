// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::core::graph::{LinkDirection, ProcessorUniqueId};

/// Reference to an input port on a processor node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputLinkPortRef {
    pub processor_id: ProcessorUniqueId,
    pub port_name: String,
}

impl InputLinkPortRef {
    /// Direction is always Input for input ports.
    pub const DIRECTION: LinkDirection = LinkDirection::Input;

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

impl fmt::Display for InputLinkPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.processor_id, self.port_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// msgpack round-trip preserves both fields. Sibling of
    /// `OutputLinkPortRef`'s round-trip test; locks the cdylib-side
    /// encode path the plugin ABI takes when forwarding
    /// `Runtime::connect` calls.
    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        let port_ref = InputLinkPortRef::new(ProcessorUniqueId::from("Pdisplay"), "video_in");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: InputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);
    }

    #[test]
    fn msgpack_round_trip_empty_port_name() {
        let port_ref = InputLinkPortRef::new(ProcessorUniqueId::from("P0"), "");
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        let back: InputLinkPortRef = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(port_ref, back);
    }
}
