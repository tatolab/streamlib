// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A port's address, as `tap` names it.
//!
//! `<runtime name>/<node>/<port>`. Processor ids and cuid2 channel names never
//! appear in it, so the middle chunk is the node's name, which is why renaming
//! a node re-addresses its ports.
//!
//! Each of the three parts is one legal address chunk on its own, checked
//! against the grammar `core::runtime::address_chunk` states rather than a
//! second copy of it here.

use std::fmt;

use crate::core::error::{Error, Result};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::runtime::address_chunk::first_reason_this_is_not_one_address_chunk;
use crate::core::runtime::what_one_address_chunk_may_be;

/// How many parts a port address is spelled in.
const PARTS_OF_A_PORT_ADDRESS: usize = 3;

/// A port on a runtime, addressed `<runtime name>/<node>/<port>`.
///
/// Carries no processor id: the runtime that owns the port resolves the node's
/// name to one of its own nodes at the moment it is asked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortAddress {
    runtime_name: String,
    processor_display_name: String,
    port_name: String,
}

impl PortAddress {
    /// The name of the runtime that owns the port.
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

impl PortAddress {
    /// Address a port, casting the node and port names the way every node and
    /// port is named, and refusing by name any part the address grammar cannot
    /// carry.
    pub fn new(
        runtime_name: impl Into<String>,
        processor_display_name: impl Into<String>,
        port_name: impl Into<String>,
    ) -> Result<Self> {
        let cast_naming_the_part = |part_name: &str, part: String| -> Result<String> {
            cast_exposed_name_to_url_safe(&part)
                .map(|cast| cast.into_owned())
                .map_err(|names_nothing| {
                    Error::InvalidLink(format!(
                        "the {part_name} {part:?} cannot be part of a port address: \
                         {names_nothing}"
                    ))
                })
        };
        let addressed = Self {
            runtime_name: runtime_name.into(),
            processor_display_name: cast_naming_the_part(
                "node name",
                processor_display_name.into(),
            )?,
            port_name: cast_naming_the_part("port name", port_name.into())?,
        };
        for (part_name, part) in [
            ("runtime name", &addressed.runtime_name),
            ("node name", &addressed.processor_display_name),
            ("port name", &addressed.port_name),
        ] {
            if let Some(reason) = first_reason_this_is_not_one_address_chunk(part) {
                return Err(Error::InvalidLink(format!(
                    "the {part_name} {part:?} in the port address {addressed} cannot be part \
                     of a port address: {reason}. {}",
                    what_one_address_chunk_may_be()
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
                "{address:?} is not a port address: it has {} parts rather than \
                 {PARTS_OF_A_PORT_ADDRESS}. {}",
                parts.len(),
                what_one_address_chunk_may_be()
            )));
        };
        Self::new(*runtime_name, *processor_display_name, *port_name)
    }

    /// Whether this address names a port on the runtime named `runtime_name`.
    pub fn names_the_runtime(&self, runtime_name: &str) -> bool {
        self.runtime_name == runtime_name
    }
}

impl fmt::Display for PortAddress {
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

    /// Every part an address hands back is one the address grammar carries,
    /// because the fields are private and `new` and `parse` are the only ways
    /// in — a struct literal cannot smuggle one past them, here or in a
    /// consumer.
    #[test]
    fn every_part_of_an_address_came_through_a_checked_constructor() {
        let addressed =
            PortAddress::parse("bench-cam-a1b2/Camera Source 2/Video").expect("a legal address");
        for part in [
            addressed.runtime_name(),
            addressed.processor_display_name(),
            addressed.port_name(),
        ] {
            assert!(
                first_reason_this_is_not_one_address_chunk(part).is_none(),
                "{part:?} reached a built address without meeting the grammar"
            );
        }
    }

    fn an_address() -> PortAddress {
        PortAddress::new("bench-cam-a1b2", "camerasource", "video").expect("a legal address")
    }

    /// The three parts render as the one address `tap` names a channel by, and
    /// read back as the same three.
    #[test]
    fn an_address_renders_as_its_three_parts_and_reads_back_as_them() {
        assert_eq!(
            an_address().to_string(),
            "bench-cam-a1b2/camerasource/video"
        );
        assert_eq!(
            PortAddress::parse("bench-cam-a1b2/camerasource/video").expect("it parses"),
            an_address()
        );
    }

    /// The node and port parts are cast the way every node and port is named,
    /// so an address spelled the way an author typed a name finds the node.
    #[test]
    fn the_node_and_port_parts_are_cast() {
        let addressed = PortAddress::new("lab-two", "Camera Source 2", "Video Out")
            .expect("a name that casts to something is legal");
        assert_eq!(addressed.to_string(), "lab-two/camera-source-2/video-out");
    }

    /// Every part is checked, and the refusal names the part, the value and the
    /// reason rather than saying the address is bad.
    #[test]
    fn each_illegal_part_is_refused_naming_the_part_and_the_reason() {
        for (part_name, refused) in [
            ("runtime name", PortAddress::new("la*b", "Cam", "video")),
            ("node name", PortAddress::new("lab", "..", "video")),
            ("port name", PortAddress::new("lab", "Cam", "?")),
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
            let refusal = PortAddress::parse(wrong)
                .expect_err("only three parts are an address")
                .to_string();
            assert!(
                refusal.contains("parts rather than 3"),
                "{wrong:?}: {refusal}"
            );
        }
    }

    /// An address naming this runtime's own name names its own port, and one
    /// naming another does not — the whole of what `tap` asks it.
    #[test]
    fn an_address_says_whether_it_names_one_runtimes_own_port() {
        assert!(an_address().names_the_runtime("bench-cam-a1b2"));
        assert!(!an_address().names_the_runtime("bench-cam-c3d4"));
    }
}
