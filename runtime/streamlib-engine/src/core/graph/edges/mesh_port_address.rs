// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A port on another runtime, as a link names it.
//!
//! `<runtime name>/<display name>/<port>` — the one address a port has on the
//! runtime mesh, and the only way a link reaches out of this runtime. Processor
//! ids and cuid2 channel names never appear on the mesh, so the middle chunk is
//! the display name, which is why renaming a processor re-addresses its ports.
//!
//! Each of the three parts is one legal key chunk on its own, checked against
//! the grammar `core::runtime::mesh_address_chunk` states rather than a second
//! copy of it here.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::core::error::{Error, Result};
use crate::core::runtime::mesh_address_chunk::first_reason_this_is_not_one_mesh_address_chunk;
use crate::core::runtime::what_one_mesh_address_chunk_may_be;

/// How many parts a mesh port address is spelled in.
const PARTS_OF_A_MESH_PORT_ADDRESS: usize = 3;

/// A port on a runtime, addressed the way the mesh addresses one.
///
/// Carries no processor id: the runtime that owns the port resolves the display
/// name to one of its own nodes at the moment it is asked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct MeshPortAddress {
    runtime_name: String,
    processor_display_name: String,
    port_name: String,
}

impl MeshPortAddress {
    /// The name the owning runtime is addressed by on the mesh.
    pub fn runtime_name(&self) -> &str {
        &self.runtime_name
    }

    /// The display name of the processor that owns the port, on that runtime.
    pub fn processor_display_name(&self) -> &str {
        &self.processor_display_name
    }

    /// The port's own name on that processor.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

/// A mesh port address exactly as it rides the wire, before anything has
/// checked it.
///
/// Its only purpose is to give [`MeshPortAddress`]'s `Deserialize` somewhere to
/// land before [`MeshPortAddress::new`] runs: a derived `Deserialize` on the
/// address itself would fill the fields straight from the wire, so a peer or a
/// stored graph could put a chunk the key grammar cannot carry into this
/// runtime's graph without ever meeting the refusal `new` exists to give.
#[derive(Deserialize)]
struct AMeshPortAddressAsItArrived {
    runtime_name: String,
    processor_display_name: String,
    port_name: String,
}

impl<'de> Deserialize<'de> for MeshPortAddress {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let arrived = AMeshPortAddressAsItArrived::deserialize(deserializer)?;
        Self::new(
            arrived.runtime_name,
            arrived.processor_display_name,
            arrived.port_name,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl MeshPortAddress {
    /// Address a port, refusing by name any part the key grammar cannot carry.
    pub fn new(
        runtime_name: impl Into<String>,
        processor_display_name: impl Into<String>,
        port_name: impl Into<String>,
    ) -> Result<Self> {
        let addressed = Self {
            runtime_name: runtime_name.into(),
            processor_display_name: processor_display_name.into(),
            port_name: port_name.into(),
        };
        for (part_name, part) in [
            ("runtime name", &addressed.runtime_name),
            ("processor display name", &addressed.processor_display_name),
            ("port name", &addressed.port_name),
        ] {
            if let Some(reason) = first_reason_this_is_not_one_mesh_address_chunk(part) {
                return Err(Error::InvalidLink(format!(
                    "the {part_name} {part:?} in the mesh port address {addressed} cannot be \
                     addressed on the mesh: {reason}. {}",
                    what_one_mesh_address_chunk_may_be()
                )));
            }
        }
        Ok(addressed)
    }

    /// Read `<runtime name>/<display name>/<port>` back into an address,
    /// refusing anything that is not exactly three legal parts.
    pub fn parse(address: &str) -> Result<Self> {
        let parts: Vec<&str> = address.split('/').collect();
        let [runtime_name, processor_display_name, port_name] = parts.as_slice() else {
            return Err(Error::InvalidLink(format!(
                "{address:?} is not a mesh port address: it has {} parts rather than \
                 {PARTS_OF_A_MESH_PORT_ADDRESS}. {}",
                parts.len(),
                what_one_mesh_address_chunk_may_be()
            )));
        };
        Self::new(*runtime_name, *processor_display_name, *port_name)
    }

    /// Whether this address names `runtime_name`'s own port — the case a
    /// runtime resolves locally rather than over the mesh.
    pub fn names_the_runtime(&self, runtime_name: &str) -> bool {
        self.runtime_name == runtime_name
    }
}

impl fmt::Display for MeshPortAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.runtime_name, self.processor_display_name, self.port_name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address that arrives over the wire meets the same refusal one built
    /// here does. Without this the derived `Deserialize` would fill the fields
    /// straight from the wire and a `*` would reach the graph, where it would
    /// match keys the addressed runtime never offered.
    #[test]
    fn an_address_that_arrives_illegal_is_refused_rather_than_deserialized() {
        let illegal = serde_json::json!({
            "runtime_name": "la*b",
            "processor_display_name": "Camera Source 2",
            "port_name": "video",
        });
        let refusal = serde_json::from_value::<MeshPortAddress>(illegal)
            .expect_err("a runtime name the key grammar cannot carry is refused on the wire")
            .to_string();
        assert!(refusal.contains("runtime name"), "{refusal}");
        assert!(refusal.contains("la*b"), "{refusal}");
    }

    /// Every part an address hands back is one the key grammar carries, because
    /// the fields are private and `new` and `parse` are the only ways in — a
    /// struct literal cannot smuggle one past them, here or in a consumer.
    #[test]
    fn every_part_of_an_address_came_through_a_checked_constructor() {
        let addressed = MeshPortAddress::parse("bench-cam-a1b2/Camera Source 2/video")
            .expect("a legal address");
        for part in [
            addressed.runtime_name(),
            addressed.processor_display_name(),
            addressed.port_name(),
        ] {
            assert!(
                first_reason_this_is_not_one_mesh_address_chunk(part).is_none(),
                "{part:?} reached a built address without meeting the grammar"
            );
        }
    }

    /// A legal address still rides the wire unchanged, so the check costs the
    /// ordinary path nothing but the refusal.
    #[test]
    fn a_legal_address_round_trips_through_the_wire_unchanged() {
        let addressed = MeshPortAddress::new("bench-cam-a1b2", "Camera Source 2", "video")
            .expect("a legal address");
        let back: MeshPortAddress =
            rmp_serde::from_slice(&rmp_serde::to_vec_named(&addressed).expect("encode"))
                .expect("decode");
        assert_eq!(back, addressed);
    }

    use super::*;

    fn an_address() -> MeshPortAddress {
        MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video").expect("a legal address")
    }

    /// The three parts render as the one address every mesh key is built from,
    /// and read back as the same three.
    #[test]
    fn an_address_renders_as_its_three_parts_and_reads_back_as_them() {
        assert_eq!(
            an_address().to_string(),
            "bench-cam-a1b2/CameraSource/video"
        );
        assert_eq!(
            MeshPortAddress::parse("bench-cam-a1b2/CameraSource/video").expect("it parses"),
            an_address()
        );
    }

    /// A display name with a space is legal on the mesh, and so it is here —
    /// the grammar refuses the five key characters and a leading `@`, nothing
    /// else.
    #[test]
    fn a_display_name_carrying_a_space_is_one_legal_part() {
        let addressed = MeshPortAddress::new("lab-two", "Camera Source 2", "video")
            .expect("a space is legal in a display name");
        assert_eq!(addressed.to_string(), "lab-two/Camera Source 2/video");
    }

    /// Every part is checked, and the refusal names the part, the value and the
    /// reason rather than saying the address is bad.
    #[test]
    fn each_illegal_part_is_refused_naming_the_part_and_the_reason() {
        for (part_name, refused) in [
            ("runtime name", MeshPortAddress::new("la*b", "Cam", "video")),
            (
                "processor display name",
                MeshPortAddress::new("lab", "@Cam", "video"),
            ),
            ("port name", MeshPortAddress::new("lab", "Cam", "vid?eo")),
        ] {
            let refusal = refused.expect_err("an illegal part is refused").to_string();
            assert!(refusal.contains(part_name), "{refusal}");
        }
    }

    /// An address that is not three parts is refused by count rather than
    /// silently read as two of them.
    #[test]
    fn an_address_that_is_not_three_parts_is_refused_by_count() {
        for wrong in ["bench/CameraSource", "bench/CameraSource/video/extra", ""] {
            let refusal = MeshPortAddress::parse(wrong)
                .expect_err("only three parts are an address")
                .to_string();
            assert!(
                refusal.contains("parts rather than 3"),
                "{wrong:?}: {refusal}"
            );
        }
    }

    /// An address naming this runtime's own name is the local case, and one
    /// naming another is not — the whole of what the resolver asks it.
    #[test]
    fn an_address_says_whether_it_names_one_runtimes_own_port() {
        assert!(an_address().names_the_runtime("bench-cam-a1b2"));
        assert!(!an_address().names_the_runtime("bench-cam-c3d4"));
    }
}
