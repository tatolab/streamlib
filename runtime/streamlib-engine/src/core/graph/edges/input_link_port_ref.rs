// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::core::graph::{LinkDirection, MeshPortAddress, ProcessorUniqueId};

/// Reference to the input port a link carries into — on this runtime, or on
/// another runtime over the mesh.
///
/// The wire shape is the discriminator and there is no tag, exactly as
/// [`OutputLinkPortRef`] spells the same two shapes: a reference on this
/// runtime carries `processor_id` and a reference on another carries
/// `runtime_name`.
///
/// The two sides are not symmetric in where they may land, and the asymmetry is
/// the whole of this type's contract: a link *in the graph* always has its
/// input here, because the runtime that owns the input is the one that applies
/// the link. A reference naming another runtime's input is therefore an
/// argument to a link *request* and never an edge's target — `connect` refuses
/// one by name, pointing at the request door.
///
/// [`OutputLinkPortRef`]: super::OutputLinkPortRef
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputLinkPortRef {
    /// A port on a processor this runtime's own graph holds.
    OnThisRuntime {
        processor_id: ProcessorUniqueId,
        port_name: String,
    },
    /// A port on a processor another runtime holds, named over the mesh.
    OnAnotherRuntime(MeshPortAddress),
}

impl InputLinkPortRef {
    /// Direction is always Input for input ports.
    pub const DIRECTION: LinkDirection = LinkDirection::Input;

    /// A port on this runtime.
    pub fn new(processor_id: impl Into<ProcessorUniqueId>, port_name: impl Into<String>) -> Self {
        Self::OnThisRuntime {
            processor_id: processor_id.into(),
            port_name: port_name.into(),
        }
    }

    /// A port on another runtime, named by its mesh address.
    pub fn on_another_runtime(address: MeshPortAddress) -> Self {
        Self::OnAnotherRuntime(address)
    }

    pub fn direction(&self) -> LinkDirection {
        Self::DIRECTION
    }

    /// The processor this port belongs to when this runtime owns it, and
    /// `None` when the port is on another runtime — there being no local node
    /// to name.
    pub fn processor_id_on_this_runtime(&self) -> Option<&ProcessorUniqueId> {
        match self {
            Self::OnThisRuntime { processor_id, .. } => Some(processor_id),
            Self::OnAnotherRuntime(_) => None,
        }
    }

    /// The port's own name, wherever the port lives.
    pub fn port_name(&self) -> &str {
        match self {
            Self::OnThisRuntime { port_name, .. } => port_name,
            Self::OnAnotherRuntime(address) => address.port_name(),
        }
    }

    /// The mesh address this port is named by, and `None` for a port on this
    /// runtime.
    pub fn mesh_port_address(&self) -> Option<&MeshPortAddress> {
        match self {
            Self::OnThisRuntime { .. } => None,
            Self::OnAnotherRuntime(address) => Some(address),
        }
    }
}

impl fmt::Display for InputLinkPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OnThisRuntime {
                processor_id,
                port_name,
            } => write!(f, "{processor_id}.{port_name}"),
            Self::OnAnotherRuntime(address) => write!(f, "{address}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_mesh_address() -> MeshPortAddress {
        MeshPortAddress::new("studio-display-9f3c", "DisplayWindow", "video")
            .expect("a legal address")
    }

    /// msgpack round-trip preserves both fields of a port on this runtime, and
    /// the encoding is unchanged by the remote variant joining it — the map
    /// this runtime's own references have always ridden.
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

    /// A port on another runtime rides the wire as its three address parts and
    /// reads back as the same reference — never as a local one with a
    /// processor id invented from the display name.
    #[test]
    fn a_port_on_another_runtime_round_trips_as_its_address() {
        let port_ref = InputLinkPortRef::on_another_runtime(a_mesh_address());
        let bytes = rmp_serde::to_vec_named(&port_ref).expect("encode");
        assert_eq!(
            rmp_serde::from_slice::<serde_json::Value>(&bytes).expect("a msgpack map"),
            serde_json::json!({
                "runtime_name": "studio-display-9f3c",
                "processor_display_name": "DisplayWindow",
                "port_name": "video",
            })
        );
        assert_eq!(
            rmp_serde::from_slice::<InputLinkPortRef>(&bytes).expect("decode"),
            port_ref
        );
    }

    /// The two arms answer the three questions every caller asks, so a caller
    /// that forgot the remote case gets `None` rather than a plausible lie.
    #[test]
    fn each_arm_answers_where_its_port_lives() {
        let on_this_runtime = InputLinkPortRef::new(ProcessorUniqueId::from("Pdisplay"), "video");
        assert_eq!(
            on_this_runtime
                .processor_id_on_this_runtime()
                .map(|id| id.as_str()),
            Some("Pdisplay")
        );
        assert_eq!(on_this_runtime.mesh_port_address(), None);
        assert_eq!(on_this_runtime.port_name(), "video");

        let on_another = InputLinkPortRef::on_another_runtime(a_mesh_address());
        assert_eq!(on_another.processor_id_on_this_runtime(), None);
        assert_eq!(on_another.mesh_port_address(), Some(&a_mesh_address()));
        assert_eq!(on_another.port_name(), "video");
    }

    /// A remote reference renders as the mesh address a reader can paste back
    /// into a link request, while a local one renders as it always has.
    #[test]
    fn a_reference_renders_as_the_address_it_was_named_by() {
        assert_eq!(
            InputLinkPortRef::new(ProcessorUniqueId::from("Pdisplay"), "video").to_string(),
            "Pdisplay.video"
        );
        assert_eq!(
            InputLinkPortRef::on_another_runtime(a_mesh_address()).to_string(),
            "studio-display-9f3c/DisplayWindow/video"
        );
    }

    /// Both ends of one link spell a remote port the same way, so a graph
    /// snapshot and every wire that carries a link read one shape whichever
    /// end is remote.
    #[test]
    fn both_ends_of_a_link_spell_a_remote_port_identically() {
        let address = a_mesh_address();
        let as_an_input = rmp_serde::to_vec_named(&InputLinkPortRef::on_another_runtime(
            address.clone(),
        ))
        .expect("encode");
        let as_an_output =
            rmp_serde::to_vec_named(&super::super::OutputLinkPortRef::on_another_runtime(address))
                .expect("encode");
        assert_eq!(as_an_input, as_an_output);
    }
}
