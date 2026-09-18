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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MeshPortAddress {
    /// The name the owning runtime is addressed by on the mesh.
    pub runtime_name: String,
    /// The display name of the processor that owns the port, on that runtime.
    pub processor_display_name: String,
    /// The port's own name on that processor.
    pub port_name: String,
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
