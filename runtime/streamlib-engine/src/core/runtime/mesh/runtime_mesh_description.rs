// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a runtime answers about itself when a peer asks, how a peer is asked,
//! and how the answer renders.
//!
//! The wire is msgpack, the same codec every bag rides, and the field names
//! are the contract — a peer of another engine version reads this document.
//!
//! Two readers ask and render: a runtime's own discovery worker, which keeps a
//! peer table `graph` renders, and the observation `streamlib nodes` makes
//! without joining the mesh at all. They share the asking and the rendering
//! from here, so a peer reads the same either way.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use zenoh::Wait;

use crate::core::json_schema::RuntimeMeshPeerOutput;
use crate::core::runtime::mesh::hosted_control_plane_endpoint::HostedControlPlaneEndpointRegistry;
use crate::core::runtime::mesh::runtime_mesh_key::{AnnouncedRuntimeIdentity, RuntimeMeshKeySpace};

/// How long a peer has to answer what it is before its description is left
/// unread until whoever asked asks again. Engine-chosen; nothing authorable.
const HOW_LONG_A_PEER_HAS_TO_DESCRIBE_ITSELF: Duration = Duration::from_secs(2);

/// The document a runtime's description queryable answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMeshDescription {
    /// The per-run id that names this runtime's logs, registry file and
    /// iceoryx2 services. Never an address.
    pub runtime_id: String,
    /// What the host calls itself, for a reader deciding which machine this is.
    pub host_name: String,
    /// The runtime's process id on that host.
    pub pid: u32,
    /// The engine crate's version.
    pub engine_version: String,
    /// One `http://<address>:<port>` per address another machine could reach
    /// this runtime's control plane at. Empty with no control plane.
    pub control_plane_urls: Vec<String>,
}

impl RuntimeMeshDescription {
    /// This runtime, as it stands right now — so a control plane hosted after
    /// the runtime was constructed shows up on the next query.
    pub fn of_this_runtime_right_now(
        runtime_id: &str,
        host_name: &str,
        hosted_control_plane: &HostedControlPlaneEndpointRegistry,
    ) -> Self {
        Self {
            runtime_id: runtime_id.to_string(),
            host_name: host_name.to_string(),
            pid: std::process::id(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            control_plane_urls: hosted_control_plane.urls_another_machine_could_reach_it_at(),
        }
    }

    /// This document on the wire.
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// What a peer answered, or the reason it could not be read.
    pub fn decode(wire_bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(wire_bytes)
    }
}

/// The channel one peer's answer arrives on, before anybody waits on it.
type AnswersFromOnePeer = zenoh::handlers::FifoChannelHandler<zenoh::query::Reply>;

/// What a peer says it is, or `None` when it did not answer in time or
/// answered something this engine cannot read.
pub(super) fn ask_a_peer_what_it_is(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    announced: &AnnouncedRuntimeIdentity,
) -> Option<RuntimeMeshDescription> {
    let answers = send_the_what_are_you_question_to(session, key_space, announced)?;
    wait_for_what_a_peer_answered(&announced.runtime_name, answers)
}

/// Ask every peer at once, then wait: one caller's whole round costs one
/// timeout rather than one per peer.
///
/// The discovery worker asks one at a time because it runs for the life of a
/// runtime and nobody waits on it. `streamlib nodes` is the other case — a
/// person is waiting — and a mesh whose tokens outlive their processes, which
/// a partition or a power loss leaves for the lease's ten seconds, would
/// otherwise cost the reader two seconds a peer.
pub(super) fn ask_every_peer_what_it_is(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    every_peer: impl IntoIterator<Item = AnnouncedRuntimeIdentity>,
) -> impl Iterator<Item = (AnnouncedRuntimeIdentity, Option<RuntimeMeshDescription>)> {
    // Collected rather than left lazy, and that is the whole mechanism: every
    // question is on the wire before the first answer is waited on.
    let asked: Vec<_> = every_peer
        .into_iter()
        .map(|announced| {
            let answers = send_the_what_are_you_question_to(session, key_space, &announced);
            (announced, answers)
        })
        .collect();

    asked.into_iter().map(|(announced, answers)| {
        let described = answers
            .and_then(|answers| wait_for_what_a_peer_answered(&announced.runtime_name, answers));
        (announced, described)
    })
}

/// Send one "what are you" query, and hand back the channel it answers on
/// without waiting — which is what lets a caller ask several at once.
fn send_the_what_are_you_question_to(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    announced: &AnnouncedRuntimeIdentity,
) -> Option<AnswersFromOnePeer> {
    session
        .get(key_space.announcement_key_for(announced))
        .timeout(HOW_LONG_A_PEER_HAS_TO_DESCRIBE_ITSELF)
        .wait()
        .inspect_err(|query_failure| {
            tracing::debug!(
                "could not ask the mesh peer {} what it is: {query_failure}",
                announced.runtime_name
            );
        })
        .ok()
}

/// Wait out one peer's answer, or `None` when it did not answer in time or
/// answered something this engine cannot read.
fn wait_for_what_a_peer_answered(
    runtime_name: &str,
    answers: AnswersFromOnePeer,
) -> Option<RuntimeMeshDescription> {
    for reply in answers {
        let Ok(answered) = reply.result() else {
            continue;
        };
        match RuntimeMeshDescription::decode(&answered.payload().to_bytes()) {
            Ok(described) => return Some(described),
            Err(unreadable) => {
                tracing::debug!(
                    "the mesh peer {runtime_name} answered something this engine cannot read: \
                     {unreadable}"
                );
            }
        }
    }
    None
}

/// One peer as a reader renders it: its name off the token, and the four the
/// peer itself answered — each absent until it does.
pub(super) fn render_one_runtime_mesh_peer(
    runtime_name: &str,
    described: Option<&RuntimeMeshDescription>,
) -> RuntimeMeshPeerOutput {
    RuntimeMeshPeerOutput {
        runtime_name: runtime_name.to_string(),
        runtime_id: described.map(|it| it.runtime_id.clone()),
        host_name: described.map(|it| it.host_name.clone()),
        engine_version: described.map(|it| it.engine_version.clone()),
        control_plane_urls: described.map(|it| it.control_plane_urls.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_description() -> RuntimeMeshDescription {
        RuntimeMeshDescription {
            runtime_id: "R0123456789".to_string(),
            host_name: "rig".to_string(),
            pid: 4321,
            engine_version: "0.25.0".to_string(),
            control_plane_urls: vec!["http://[2001:db8::1]:9000".to_string()],
        }
    }

    /// The document survives the wire whole.
    #[test]
    fn a_description_round_trips_through_msgpack() {
        let encoded = a_description().encode().expect("a description encodes");
        assert_eq!(
            RuntimeMeshDescription::decode(&encoded).expect("it decodes"),
            a_description()
        );
    }

    /// The keys are the contract, so they ride the wire as names rather than
    /// as positions a peer of another engine version would read wrong.
    #[test]
    fn the_wire_carries_the_field_names_rather_than_positions() {
        let encoded = a_description().encode().expect("a description encodes");
        let as_a_map: serde_json::Value =
            rmp_serde::from_slice(&encoded).expect("the document is a msgpack map");
        let named: Vec<&str> = as_a_map
            .as_object()
            .expect("a named map")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            named,
            [
                "runtime_id",
                "host_name",
                "pid",
                "engine_version",
                "control_plane_urls"
            ]
        );
    }

    /// A control plane hosted after the runtime was constructed reaches the
    /// next answer, because the document is built at query time.
    #[test]
    fn a_control_plane_hosted_later_reaches_the_next_answer() {
        let hosted_control_plane = HostedControlPlaneEndpointRegistry::default();

        let before = RuntimeMeshDescription::of_this_runtime_right_now(
            "R0123456789",
            "rig",
            &hosted_control_plane,
        );
        assert!(before.control_plane_urls.is_empty());

        hosted_control_plane.record_what_the_control_plane_bound("198.51.100.7", 9000);
        let after = RuntimeMeshDescription::of_this_runtime_right_now(
            "R0123456789",
            "rig",
            &hosted_control_plane,
        );
        assert_eq!(after.control_plane_urls, ["http://198.51.100.7:9000"]);
    }

    /// The engine version a peer reads is this crate's own, not a string
    /// somebody wrote down beside it.
    #[test]
    fn the_engine_version_is_this_crates_own() {
        let described = RuntimeMeshDescription::of_this_runtime_right_now(
            "R0123456789",
            "rig",
            &HostedControlPlaneEndpointRegistry::default(),
        );
        assert_eq!(described.engine_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(described.pid, std::process::id());
    }
}
