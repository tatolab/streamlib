// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What one runtime asks another to do with a link, on the wire.
//!
//! The runtime that owns an input is the one that applies a link into it, so a
//! push and a third-party wiring are the same message: a request to that
//! runtime, which applies it through the same `connect` and the same refusals a
//! link it wired itself would meet. One document serves both, and `disconnect`
//! over the mesh is the same document with another operation.
//!
//! The wire is msgpack and the field names are the contract, as everything else
//! the mesh sends is — a peer of another engine version reads this document far
//! enough to refuse it by version.
//!
//! The document is flat and its fields optional because the wire is the
//! contract; [`ALinkRequestOnTheMesh::what_it_asks_for`] is where it becomes a
//! shape the rest of the engine cannot get wrong.

use serde::{Deserialize, Serialize};

use crate::core::graph::{LinkUniqueId, MeshPortAddress};
use crate::core::json_schema::LinkStateOutput;
use crate::core::runtime::mesh::link_request_unique_id::LinkRequestUniqueId;

/// Which of the two things a link request asks for, as it rides the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhichOperationALinkRequestNames {
    /// Apply a link from one port into one of this runtime's inputs.
    Connect,
    /// Remove a link this runtime holds.
    Disconnect,
}

/// One runtime's request to another, exactly as it rides the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ALinkRequestOnTheMesh {
    /// Which operation is being asked for.
    pub operation: WhichOperationALinkRequestNames,
    /// The requester's id for this request, carried unchanged by every resend.
    pub link_request_id: LinkRequestUniqueId,
    /// The port the link carries from. Present on a `connect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_address: Option<MeshPortAddress>,
    /// The port the link carries into, which is on the answering runtime.
    /// Present on a `connect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_address: Option<MeshPortAddress>,
    /// The link to remove. Present on a `disconnect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<LinkUniqueId>,
    /// The runtime that asked, which is what the applied link renders as its
    /// creator.
    pub requester_runtime_name: String,
    /// The engine the requester runs. Pre-1.0 there is no wire between two
    /// engine versions, so a mismatch is refused rather than attempted.
    pub engine_version: String,
}

/// What a request asks for, once it has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatALinkRequestAsksFor {
    /// Apply a link between these two ports.
    ApplyingALink {
        source_address: MeshPortAddress,
        destination_address: MeshPortAddress,
    },
    /// Remove this link.
    RemovingALink { link_id: LinkUniqueId },
}

impl ALinkRequestOnTheMesh {
    /// Ask `destination_address`'s runtime to carry `source_address` into it.
    pub fn asking_for_a_link(
        link_request_id: LinkRequestUniqueId,
        source_address: MeshPortAddress,
        destination_address: MeshPortAddress,
        requester_runtime_name: impl Into<String>,
    ) -> Self {
        Self {
            operation: WhichOperationALinkRequestNames::Connect,
            link_request_id,
            source_address: Some(source_address),
            destination_address: Some(destination_address),
            link_id: None,
            requester_runtime_name: requester_runtime_name.into(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Ask a runtime to remove the link `link_id` names.
    pub fn asking_for_a_link_to_go(
        link_request_id: LinkRequestUniqueId,
        link_id: LinkUniqueId,
        requester_runtime_name: impl Into<String>,
    ) -> Self {
        Self {
            operation: WhichOperationALinkRequestNames::Disconnect,
            link_request_id,
            source_address: None,
            destination_address: None,
            link_id: Some(link_id),
            requester_runtime_name: requester_runtime_name.into(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// This document on the wire.
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// What a peer asked, or the reason it could not be read.
    pub fn decode(wire_bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(wire_bytes)
    }

    /// What this request asks for, or why it cannot be answered.
    ///
    /// The version check runs first: a document from another engine version may
    /// be missing fields this one requires, so refusing by version before
    /// reading the rest is what makes the refusal say the useful thing.
    pub fn what_it_asks_for(
        &self,
        this_engines_version: &str,
    ) -> Result<WhatALinkRequestAsksFor, String> {
        if self.engine_version != this_engines_version {
            return Err(format!(
                "the runtime {} runs engine {} and this one runs engine {this_engines_version}. \
                 Before 1.0 there is no wire between two engine versions, so no link is applied \
                 across one.",
                self.requester_runtime_name, self.engine_version
            ));
        }
        match self.operation {
            WhichOperationALinkRequestNames::Connect => {
                let (Some(source_address), Some(destination_address)) =
                    (self.source_address.clone(), self.destination_address.clone())
                else {
                    return Err(format!(
                        "the link request {} asks for a link and names {}, where a link needs \
                         both a `source_address` and a `destination_address`",
                        self.link_request_id,
                        what_it_named(
                            self.source_address.is_some(),
                            self.destination_address.is_some()
                        )
                    ));
                };
                Ok(WhatALinkRequestAsksFor::ApplyingALink {
                    source_address,
                    destination_address,
                })
            }
            WhichOperationALinkRequestNames::Disconnect => {
                let Some(link_id) = self.link_id.clone() else {
                    return Err(format!(
                        "the link request {} asks for a link to go and names no `link_id`",
                        self.link_request_id
                    ));
                };
                Ok(WhatALinkRequestAsksFor::RemovingALink { link_id })
            }
        }
    }
}

/// Which halves of a connect request's pair of addresses arrived.
fn what_it_named(a_source: bool, a_destination: bool) -> &'static str {
    match (a_source, a_destination) {
        (true, true) => "both",
        (true, false) => "only a source",
        (false, true) => "only a destination",
        (false, false) => "neither",
    }
}

/// What a runtime answers a request it applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatALinkRequestWasAnswered {
    /// The link the answering runtime holds for this request — the one it just
    /// applied, or the one an earlier send of the same request already made.
    pub link_id: LinkUniqueId,
    /// How that runtime's own `graph` reads the link right now.
    pub state: LinkStateOutput,
}

impl WhatALinkRequestWasAnswered {
    /// This answer on the wire.
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// What a runtime answered, or the reason it could not be read.
    pub fn decode(wire_bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(wire_bytes)
    }
}

/// Why a runtime refused a request, in its own words.
///
/// Carries the refusing runtime's name because a reply cannot be attributed to
/// the peer that sent it: `Reply::replier_id` is behind Zenoh's `unstable`
/// feature, which this build does not enable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhyALinkRequestWasRefused {
    /// The runtime that refused.
    pub refused_by_runtime_name: String,
    /// Why, in terms the requester can act on.
    pub reason: String,
}

impl WhyALinkRequestWasRefused {
    /// A refusal from `refused_by_runtime_name`.
    pub fn from_the_runtime_named(
        refused_by_runtime_name: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            refused_by_runtime_name: refused_by_runtime_name.into(),
            reason: reason.into(),
        }
    }

    /// This refusal on the wire.
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// Why a runtime refused, or `None` when these bytes are not a refusal this
    /// engine wrote.
    ///
    /// The `None` arm is load-bearing rather than defensive: Zenoh synthesises a
    /// timed-out query as an error reply shaped exactly like a real refusal, so
    /// "the error payload decodes as this document" is how silence is told apart
    /// from a runtime that answered no.
    pub fn decode(wire_bytes: &[u8]) -> Option<Self> {
        rmp_serde::from_slice(wire_bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_source() -> MeshPortAddress {
        MeshPortAddress::new("bench-cam-a1b2", "CameraSource", "video").expect("a legal address")
    }

    fn a_destination() -> MeshPortAddress {
        MeshPortAddress::new("studio-display-9f3c", "DisplayWindow", "video")
            .expect("a legal address")
    }

    fn a_connect_request() -> ALinkRequestOnTheMesh {
        ALinkRequestOnTheMesh::asking_for_a_link(
            LinkRequestUniqueId::from("LRabc123"),
            a_source(),
            a_destination(),
            "bench-cam-a1b2",
        )
    }

    /// Both documents survive the wire whole.
    #[test]
    fn a_request_and_its_answer_round_trip_through_msgpack() {
        let request = a_connect_request();
        assert_eq!(
            ALinkRequestOnTheMesh::decode(&request.encode().expect("encode")).expect("decode"),
            request
        );

        let answered = WhatALinkRequestWasAnswered {
            link_id: LinkUniqueId::from("Labc".to_string()),
            state: LinkStateOutput::AwaitingRemote,
        };
        assert_eq!(
            WhatALinkRequestWasAnswered::decode(&answered.encode().expect("encode"))
                .expect("decode"),
            answered
        );
    }

    /// The keys are the contract, so they ride the wire as names rather than as
    /// positions a peer of another engine version would read wrong — and an
    /// operation's own fields are the only ones it carries.
    #[test]
    fn the_wire_carries_the_field_names_rather_than_positions() {
        let encoded = a_connect_request().encode().expect("encode");
        assert_eq!(
            rmp_serde::from_slice::<serde_json::Value>(&encoded).expect("a msgpack map"),
            serde_json::json!({
                "operation": "connect",
                "link_request_id": "LRabc123",
                "source_address": {
                    "runtime_name": "bench-cam-a1b2",
                    "processor_display_name": "CameraSource",
                    "port_name": "video",
                },
                "destination_address": {
                    "runtime_name": "studio-display-9f3c",
                    "processor_display_name": "DisplayWindow",
                    "port_name": "video",
                },
                "requester_runtime_name": "bench-cam-a1b2",
                "engine_version": env!("CARGO_PKG_VERSION"),
            })
        );

        let removing = ALinkRequestOnTheMesh::asking_for_a_link_to_go(
            LinkRequestUniqueId::from("LRabc123"),
            LinkUniqueId::from("Labc".to_string()),
            "agent-wiring-e5f6",
        );
        let as_a_map: serde_json::Value =
            rmp_serde::from_slice(&removing.encode().expect("encode")).expect("a msgpack map");
        assert_eq!(as_a_map["operation"], "disconnect");
        assert_eq!(as_a_map["link_id"], "Labc");
        assert!(
            as_a_map.get("source_address").is_none(),
            "a disconnect names no ports: {as_a_map}"
        );
    }

    /// A request asking for a link says what it is asking for, and so does one
    /// asking for a link to go.
    #[test]
    fn a_readable_request_says_which_of_the_two_things_it_asks_for() {
        let this_version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            a_connect_request().what_it_asks_for(this_version),
            Ok(WhatALinkRequestAsksFor::ApplyingALink {
                source_address: a_source(),
                destination_address: a_destination(),
            })
        );
        assert_eq!(
            ALinkRequestOnTheMesh::asking_for_a_link_to_go(
                LinkRequestUniqueId::new(),
                LinkUniqueId::from("Labc".to_string()),
                "agent-wiring-e5f6",
            )
            .what_it_asks_for(this_version),
            Ok(WhatALinkRequestAsksFor::RemovingALink {
                link_id: LinkUniqueId::from("Labc".to_string()),
            })
        );
    }

    /// A requester on another engine version is refused naming both versions,
    /// before anything else about the document is read — the offered-port
    /// refusal's rule, applied to a request.
    #[test]
    fn a_request_from_another_engine_version_is_refused_naming_both_versions() {
        let mut from_another_engine = a_connect_request();
        from_another_engine.engine_version = "0.0.1-not-this-one".to_string();
        // Unreadable in every other way too, so the version is provably what
        // the refusal is about rather than what it happened to reach first.
        from_another_engine.source_address = None;

        let refusal = from_another_engine
            .what_it_asks_for("9.9.9")
            .expect_err("another engine version is refused");
        assert!(refusal.contains("0.0.1-not-this-one"), "{refusal}");
        assert!(refusal.contains("9.9.9"), "{refusal}");
    }

    /// A request whose operation and fields disagree is refused by name rather
    /// than half-applied.
    #[test]
    fn a_request_missing_the_fields_its_operation_needs_is_refused_by_name() {
        let this_version = env!("CARGO_PKG_VERSION");

        let mut no_destination = a_connect_request();
        no_destination.destination_address = None;
        let refusal = no_destination
            .what_it_asks_for(this_version)
            .expect_err("a link needs both ends");
        assert!(refusal.contains("LRabc123"), "{refusal}");
        assert!(refusal.contains("only a source"), "{refusal}");

        let mut no_link = ALinkRequestOnTheMesh::asking_for_a_link_to_go(
            LinkRequestUniqueId::from("LRxyz789"),
            LinkUniqueId::from("Labc".to_string()),
            "agent-wiring-e5f6",
        );
        no_link.link_id = None;
        let refusal = no_link
            .what_it_asks_for(this_version)
            .expect_err("a disconnect needs a link id");
        assert!(refusal.contains("LRxyz789"), "{refusal}");
        assert!(refusal.contains("link_id"), "{refusal}");
    }

    /// A refusal round-trips and names who refused, and bytes that are not one
    /// read as no refusal at all — which is what tells a real refusal apart
    /// from the error reply Zenoh synthesises for a query that timed out.
    #[test]
    fn a_refusal_names_who_refused_and_nothing_else_reads_as_one() {
        let refused = WhyALinkRequestWasRefused::from_the_runtime_named(
            "studio-display-9f3c",
            "no processor on this runtime is displayed as \"DisplayWindow\"",
        );
        assert_eq!(
            WhyALinkRequestWasRefused::decode(&refused.encode().expect("encode")),
            Some(refused)
        );
        assert_eq!(WhyALinkRequestWasRefused::decode(b"Timeout"), None);
        assert_eq!(WhyALinkRequestWasRefused::decode(&[]), None);
    }
}
