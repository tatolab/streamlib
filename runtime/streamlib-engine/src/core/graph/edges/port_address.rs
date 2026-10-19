// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A port's address: `<machine>/<stream>/<node>/<port>`, right-anchored.
//!
//! Leading parts omitted mean here: `node/port` is a port in this stream,
//! `stream/node/port` one on this machine. Processor ids and channel names
//! never appear in it, so a node is named by its name, which is why renaming a
//! node re-addresses its ports. Every part is cast the way every exposed name
//! is, so each is one address chunk as `core::runtime::address_chunk` states.

use std::fmt;

use crate::core::error::{Error, Result};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::runtime::{MachineName, what_one_address_chunk_may_be};

/// The separator between the parts of an address.
const PORT_ADDRESS_PART_SEPARATOR: char = '/';

/// A port, addressed `[<machine>/][<stream>/]<node>/<port>`, every part cast.
///
/// Carries no processor id: whatever owns the port resolves the node's name to
/// one of its own nodes at the moment it is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortAddress {
    machine: Option<String>,
    stream: Option<String>,
    node: String,
    port: String,
}

impl PortAddress {
    /// Read an address right-anchored — `node/port`, `stream/node/port` or
    /// `machine/stream/node/port` — casting every part, and refusing by name
    /// any other count of parts or a part that casts to nothing.
    pub fn parse(address: &str) -> Result<Self> {
        let written_parts: Vec<&str> = address.split(PORT_ADDRESS_PART_SEPARATOR).collect();
        match written_parts.as_slice() {
            [node, port] => Self::from_its_parts(None, None, node, port),
            [stream, node, port] => Self::from_its_parts(None, Some(stream), node, port),
            [machine, stream, node, port] => {
                Self::from_its_parts(Some(machine), Some(stream), node, port)
            }
            _ => {
                let part_count = written_parts.len();
                Err(Error::InvalidPortAddress(format!(
                    "`{address}` is spelled in {part_count} part{}, and an address is \
                     `node/port`, `stream/node/port` or `machine/stream/node/port`. {}",
                    if part_count == 1 { "" } else { "s" },
                    what_one_address_chunk_may_be()
                )))
            }
        }
    }

    /// The address a graph's link end writes in parts — machine and stream
    /// each absent when it means here — every part cast, refused naming a
    /// part that casts to nothing, or a machine written without its stream.
    pub fn from_its_parts(
        machine: Option<&str>,
        stream: Option<&str>,
        node: &str,
        port: &str,
    ) -> Result<Self> {
        let address: Vec<&str> = [machine, stream, Some(node), Some(port)]
            .into_iter()
            .flatten()
            .collect();
        let address = address.join(&PORT_ADDRESS_PART_SEPARATOR.to_string());
        if let (Some(machine), None) = (machine, stream) {
            return Err(Error::InvalidPortAddress(format!(
                "`{address}` names the machine `{machine}` and no stream, and an address naming \
                 a machine is `machine/stream/node/port`"
            )));
        }
        let cast_part = |written_part: Option<&str>, part_name: &str| {
            written_part
                .map(|written_part| one_part_of_an_address_cast(written_part, part_name, &address))
                .transpose()
        };
        Ok(Self {
            machine: cast_part(machine, "machine")?,
            stream: cast_part(stream, "stream")?,
            node: one_part_of_an_address_cast(node, "node", &address)?,
            port: one_part_of_an_address_cast(port, "port", &address)?,
        })
    }

    /// The machine the address names, `None` when it means this one.
    pub fn machine(&self) -> Option<&str> {
        self.machine.as_deref()
    }

    /// The stream the address names, `None` when it means this one.
    pub fn stream(&self) -> Option<&str> {
        self.stream.as_deref()
    }

    /// The stream the address names, when it names one other than `this_stream`.
    pub fn the_stream_it_names_other_than(&self, this_stream: &str) -> Option<&str> {
        self.stream().filter(|stream| *stream != this_stream)
    }

    /// The name of the node that owns the port.
    pub fn node_name(&self) -> &str {
        &self.node
    }

    /// The port's own name on that node.
    pub fn port_name(&self) -> &str {
        &self.port
    }

    /// Refuse an address whose machine part names a machine other than
    /// `this_machine`: nothing links two machines until the sharing step.
    pub fn refuse_when_it_names_another_machine(&self, this_machine: &MachineName) -> Result<()> {
        match self.machine() {
            Some(machine) if !this_machine.names_this_machine(machine) => {
                Err(Error::PortAddressNamesAnotherMachine {
                    address: self.to_string(),
                    machine: machine.to_string(),
                    this_machine: this_machine.name().to_string(),
                })
            }
            _ => Ok(()),
        }
    }
}

/// `written_part`, the `part_name` part of `address`, cast; refused naming
/// the part and the address when it casts to nothing.
fn one_part_of_an_address_cast(
    written_part: &str,
    part_name: &str,
    address: &str,
) -> Result<String> {
    cast_exposed_name_to_url_safe(written_part)
        .map(|cast| cast.into_owned())
        .map_err(|names_nothing| {
            Error::InvalidPortAddress(format!(
                "the {part_name} part `{written_part}` of the address `{address}` names nothing: \
                 {names_nothing}"
            ))
        })
}

impl fmt::Display for PortAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for leading_part in [&self.machine, &self.stream].into_iter().flatten() {
            write!(formatter, "{leading_part}{PORT_ADDRESS_PART_SEPARATOR}")?;
        }
        write!(
            formatter,
            "{}{PORT_ADDRESS_PART_SEPARATOR}{}",
            self.node, self.port
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::address_chunk::first_reason_this_is_not_one_address_chunk;

    /// One case of the fixture the engine and the builder both read.
    #[derive(serde::Deserialize)]
    struct PortAddressCase {
        address: String,
        #[serde(default)]
        machine: Option<String>,
        #[serde(default)]
        stream: Option<String>,
        #[serde(default)]
        node: Option<String>,
        #[serde(default)]
        port: Option<String>,
        #[serde(default)]
        refused: bool,
    }

    fn port_address_cases() -> Vec<PortAddressCase> {
        let cases: Vec<PortAddressCase> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/port_address_cases.json"
        ))
        .expect("the port address fixture parses");
        assert!(!cases.is_empty());
        cases
    }

    /// Every case of the fixture parses to the parts it names, or is
    /// refused as an invalid address.
    #[test]
    fn every_fixture_case_parses_as_written() {
        for case in port_address_cases() {
            let parsed = PortAddress::parse(&case.address);
            if case.refused {
                assert!(
                    matches!(parsed, Err(Error::InvalidPortAddress(_))),
                    "{:?} should be refused, got {parsed:?}",
                    case.address
                );
                continue;
            }
            let parsed = parsed.unwrap_or_else(|refusal| panic!("{:?}: {refusal}", case.address));
            assert_eq!(
                parsed.machine(),
                case.machine.as_deref(),
                "{:?}",
                case.address
            );
            assert_eq!(
                parsed.stream(),
                case.stream.as_deref(),
                "{:?}",
                case.address
            );
            assert_eq!(
                Some(parsed.node_name()),
                case.node.as_deref(),
                "{:?}",
                case.address
            );
            assert_eq!(
                Some(parsed.port_name()),
                case.port.as_deref(),
                "{:?}",
                case.address
            );
        }
    }

    /// Every part an address hands back is one address chunk, and its rendering
    /// reads back as the same address.
    #[test]
    fn every_part_is_one_chunk_and_an_address_reads_back_as_itself() {
        for case in port_address_cases()
            .into_iter()
            .filter(|case| !case.refused)
        {
            let parsed = PortAddress::parse(&case.address).expect("an unrefused case parses");
            for part in [parsed.machine(), parsed.stream()]
                .into_iter()
                .flatten()
                .chain([parsed.node_name(), parsed.port_name()])
            {
                assert_eq!(
                    first_reason_this_is_not_one_address_chunk(part),
                    None,
                    "{part:?} reached a parsed address without being one chunk"
                );
            }
            assert_eq!(
                PortAddress::parse(&parsed.to_string()).expect("a rendered address parses"),
                parsed
            );
        }
    }

    /// An address renders the parts it was written with and no others.
    #[test]
    fn an_address_renders_only_the_parts_it_was_written_with() {
        for (written, rendered) in [
            ("camera/video", "camera/video"),
            ("Main/Camera/Video", "main/camera/video"),
            (
                "desk.tail2950db.ts.net/main/camera/video",
                "desk.tail2950db.ts.net/main/camera/video",
            ),
        ] {
            assert_eq!(PortAddress::parse(written).unwrap().to_string(), rendered);
        }
    }

    /// A refusal names the part that casts to nothing and the address, or the
    /// count of parts, rather than saying the address is bad.
    #[test]
    fn a_refusal_names_the_part_or_the_count() {
        for (address, named) in [
            ("main/!!!/video", "the node part `!!!`"),
            ("../camera/video", "the stream part `..`"),
            ("desk/main/camera/.", "the port part `.`"),
            ("video", "1 part,"),
            ("a/b/c/d/e", "5 parts"),
        ] {
            let refusal = PortAddress::parse(address)
                .expect_err("refused")
                .to_string();
            assert!(refusal.contains(named), "{named:?} not in {refusal}");
            assert!(refusal.contains(address), "{address:?} not in {refusal}");
        }
    }

    /// An address written in parts reads as the same address written whole,
    /// and a machine written without its stream is refused.
    #[test]
    fn an_address_written_in_parts_reads_as_the_address_written_whole() {
        for (machine, stream, written_whole) in [
            (None, None, "Camera/Video"),
            (None, Some("Main"), "Main/Camera/Video"),
            (Some("desk"), Some("Main"), "desk/Main/Camera/Video"),
        ] {
            assert_eq!(
                PortAddress::from_its_parts(machine, stream, "Camera", "Video").unwrap(),
                PortAddress::parse(written_whole).unwrap()
            );
        }
        let refusal = PortAddress::from_its_parts(Some("desk"), None, "camera", "video")
            .expect_err("a machine without its stream is refused")
            .to_string();
        assert!(refusal.contains("`desk/camera/video`"), "{refusal}");
        let refusal = PortAddress::from_its_parts(None, Some(".."), "camera", "video")
            .expect_err("a part that casts to nothing is refused")
            .to_string();
        assert!(refusal.contains("the stream part `..`"), "{refusal}");
    }

    /// An address naming this machine — by any spelling that casts to its
    /// name — or no machine at all names a port here; one naming another
    /// machine is refused naming both machines.
    #[test]
    fn an_address_naming_another_machine_is_refused_naming_both_machines() {
        let this_machine = MachineName::from_a_hostname("desk").expect("`desk` names a machine");
        for here in [
            "camera/video",
            "main/camera/video",
            "desk/main/camera/video",
            "Desk/main/camera/video",
        ] {
            PortAddress::parse(here)
                .unwrap()
                .refuse_when_it_names_another_machine(&this_machine)
                .unwrap_or_else(|refusal| panic!("{here}: {refusal}"));
        }

        let refusal = PortAddress::parse("rig/main/camera/video")
            .unwrap()
            .refuse_when_it_names_another_machine(&this_machine)
            .expect_err("another machine is refused");
        assert!(
            matches!(
                &refusal,
                Error::PortAddressNamesAnotherMachine { machine, this_machine, .. }
                    if machine == "rig" && this_machine == "desk"
            ),
            "{refusal:?}"
        );
        let refusal = refusal.to_string();
        for named in ["`rig/main/camera/video`", "`rig`", "`desk`", "sharing step"] {
            assert!(refusal.contains(named), "{named:?} not in {refusal}");
        }
    }
}
