// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::core::error::Result;
use crate::core::graph::{OutputLinkPortRef, cast_exposed_name_to_url_safe};

/// An output port of another stream loaded on this machine, by cast names.
///
/// Held by name, never by processor id: the other stream's ids change on
/// every load of it and every re-add of the node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "OutputPortInAnotherStreamOnThisMachineAsWritten")]
pub struct OutputPortInAnotherStreamOnThisMachine {
    stream_name: String,
    node_name: String,
    port_name: String,
}

/// The names of an [`OutputPortInAnotherStreamOnThisMachine`] as a wire
/// writes them, each cast through [`OutputPortInAnotherStreamOnThisMachine::new`].
#[derive(Deserialize)]
struct OutputPortInAnotherStreamOnThisMachineAsWritten {
    stream_name: String,
    node_name: String,
    port_name: String,
}

impl TryFrom<OutputPortInAnotherStreamOnThisMachineAsWritten>
    for OutputPortInAnotherStreamOnThisMachine
{
    type Error = crate::core::error::Error;

    fn try_from(as_written: OutputPortInAnotherStreamOnThisMachineAsWritten) -> Result<Self> {
        Self::new(
            &as_written.stream_name,
            &as_written.node_name,
            &as_written.port_name,
        )
    }
}

impl OutputPortInAnotherStreamOnThisMachine {
    /// Output port `port_name` of node `node_name` in the stream
    /// `stream_name`, every name cast; refused naming a name that casts to
    /// nothing.
    pub fn new(stream_name: &str, node_name: &str, port_name: &str) -> Result<Self> {
        Ok(Self {
            stream_name: cast_exposed_name_to_url_safe(stream_name)?.into_owned(),
            node_name: cast_exposed_name_to_url_safe(node_name)?.into_owned(),
            port_name: cast_exposed_name_to_url_safe(port_name)?.into_owned(),
        })
    }

    /// The cast name of the stream that holds the port.
    pub fn stream_name(&self) -> &str {
        &self.stream_name
    }

    /// The cast name of the node that owns the port.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// The port's cast name on that node.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

impl fmt::Display for OutputPortInAnotherStreamOnThisMachine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/{}/{}",
            self.stream_name, self.node_name, self.port_name
        )
    }
}

/// The output port a link carries from.
///
/// Untagged, so a link in its own stream rides the wire as the
/// `{processor_id, port_name}` map it always has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OutputPortALinkCarriesFrom {
    /// A port on a node of the link's own stream.
    InThisStream(OutputLinkPortRef),
    /// A port of another stream loaded on this machine.
    InAnotherStreamOnThisMachine(OutputPortInAnotherStreamOnThisMachine),
}

impl OutputPortALinkCarriesFrom {
    /// The port, when it is on a node of the link's own stream.
    pub fn in_this_stream(&self) -> Option<&OutputLinkPortRef> {
        match self {
            Self::InThisStream(port) => Some(port),
            Self::InAnotherStreamOnThisMachine(_) => None,
        }
    }

    /// The port, when it is in another stream on this machine.
    pub fn in_another_stream_on_this_machine(
        &self,
    ) -> Option<&OutputPortInAnotherStreamOnThisMachine> {
        match self {
            Self::InThisStream(_) => None,
            Self::InAnotherStreamOnThisMachine(port) => Some(port),
        }
    }

    /// The port's own name on its node.
    pub fn port_name(&self) -> &str {
        match self {
            Self::InThisStream(port) => port.port_name(),
            Self::InAnotherStreamOnThisMachine(port) => port.port_name(),
        }
    }
}

impl From<OutputLinkPortRef> for OutputPortALinkCarriesFrom {
    fn from(port: OutputLinkPortRef) -> Self {
        Self::InThisStream(port)
    }
}

impl From<OutputPortInAnotherStreamOnThisMachine> for OutputPortALinkCarriesFrom {
    fn from(port: OutputPortInAnotherStreamOnThisMachine) -> Self {
        Self::InAnotherStreamOnThisMachine(port)
    }
}

impl fmt::Display for OutputPortALinkCarriesFrom {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InThisStream(port) => port.fmt(formatter),
            Self::InAnotherStreamOnThisMachine(port) => port.fmt(formatter),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::ProcessorUniqueId;

    /// A port of this stream keeps the `{processor_id, port_name}` map it has
    /// always ridden, and a port of another stream reads back as itself.
    #[test]
    fn each_kind_of_source_round_trips_through_msgpack_in_its_own_shape() {
        let in_this_stream = OutputPortALinkCarriesFrom::from(OutputLinkPortRef::new(
            ProcessorUniqueId::from("Pcam"),
            "video",
        ));
        let bytes = rmp_serde::to_vec_named(&in_this_stream).expect("encode");
        assert_eq!(
            rmp_serde::from_slice::<serde_json::Value>(&bytes).expect("a msgpack map"),
            serde_json::json!({ "processor_id": "Pcam", "port_name": "video" })
        );
        assert_eq!(
            rmp_serde::from_slice::<OutputPortALinkCarriesFrom>(&bytes).expect("decode"),
            in_this_stream
        );

        let in_another_stream = OutputPortALinkCarriesFrom::from(
            OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "video").unwrap(),
        );
        let bytes = rmp_serde::to_vec_named(&in_another_stream).expect("encode");
        assert_eq!(
            rmp_serde::from_slice::<OutputPortALinkCarriesFrom>(&bytes).expect("decode"),
            in_another_stream
        );
    }

    /// Every name is cast, so a port is named alike however it was spelled,
    /// and it renders as its address on this machine.
    #[test]
    fn a_port_in_another_stream_is_cast_and_renders_as_its_address() {
        let port =
            OutputPortInAnotherStreamOnThisMachine::new("Camera Rig", "Camera", "Video").unwrap();
        assert_eq!(port.to_string(), "camera-rig/camera/video");
        assert_eq!(
            OutputPortALinkCarriesFrom::from(port).to_string(),
            "camera-rig/camera/video"
        );
        assert!(OutputPortInAnotherStreamOnThisMachine::new("!!!", "camera", "video").is_err());
    }

    /// A port read off the wire is cast as one built by name is, and one whose
    /// name casts to nothing is refused rather than held uncast.
    #[test]
    fn a_port_in_another_stream_read_off_the_wire_is_cast_or_refused() {
        let read: OutputPortInAnotherStreamOnThisMachine =
            serde_json::from_value(serde_json::json!({
                "stream_name": "Camera Rig",
                "node_name": "Camera",
                "port_name": "Video"
            }))
            .expect("every name casts");
        assert_eq!(
            read,
            OutputPortInAnotherStreamOnThisMachine::new("camera-rig", "camera", "video").unwrap()
        );
        assert!(
            serde_json::from_value::<OutputPortInAnotherStreamOnThisMachine>(serde_json::json!({
                "stream_name": "!!!",
                "node_name": "camera",
                "port_name": "video"
            }))
            .is_err()
        );
    }
}
