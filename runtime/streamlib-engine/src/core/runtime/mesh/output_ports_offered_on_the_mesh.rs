// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which of a runtime's output ports a peer may pull, and how it asks.
//!
//! Answered at query time and never announced: a graph changes while it runs,
//! and a list put on the mesh once would be a list of what used to be there.
//!
//! The wire is msgpack, the same codec every bag rides, and the field names are
//! the contract — a peer of another engine version reads this document.

use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use zenoh::Wait;

use crate::core::runtime::mesh::runtime_mesh_key::{ReaderOfAnOutputPort, RuntimeMeshKeySpace};

/// How long a runtime has to say which ports it offers before the asking side
/// gives up and asks again on its next pass. Engine-chosen; nothing authorable.
const HOW_LONG_A_RUNTIME_HAS_TO_LIST_ITS_PORTS: Duration = Duration::from_secs(2);

/// One output port a runtime offers to the mesh.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OutputPortOfferedOnTheMesh {
    /// The display name of the processor that owns the port — the middle chunk
    /// of the port's mesh address.
    pub processor_display_name: String,
    /// The port's own name.
    pub port_name: String,
}

impl std::fmt::Display for OutputPortOfferedOnTheMesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.processor_display_name, self.port_name)
    }
}

impl From<&ReaderOfAnOutputPort> for OutputPortOfferedOnTheMesh {
    fn from(reader: &ReaderOfAnOutputPort) -> Self {
        Self {
            processor_display_name: reader.processor_display_name.clone(),
            port_name: reader.port_name.clone(),
        }
    }
}

/// The document a runtime answers with when a peer asks what it offers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutputPortsOfferedOnTheMesh {
    /// Every output port in the runtime's graph at the moment it was asked.
    pub ports: Vec<OutputPortOfferedOnTheMesh>,
}

impl OutputPortsOfferedOnTheMesh {
    /// This document on the wire.
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// What a runtime answered, or the reason it could not be read.
    pub fn decode(wire_bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(wire_bytes)
    }

    /// Whether `port_name` on `processor_display_name` is one of these.
    pub fn offers(&self, processor_display_name: &str, port_name: &str) -> bool {
        self.ports.iter().any(|offered| {
            offered.processor_display_name == processor_display_name
                && offered.port_name == port_name
        })
    }

    /// Every offered port, rendered for a refusal that has to say what *is*
    /// offered rather than only what is not.
    pub fn listed_for_a_refusal(&self) -> String {
        if self.ports.is_empty() {
            return "nothing".to_string();
        }
        let mut listed: Vec<String> = self.ports.iter().map(|port| port.to_string()).collect();
        listed.sort();
        listed.join(", ")
    }
}

/// How this runtime answers a peer that asks what it offers, and how the mesh
/// reaches one of those ports' channels.
///
/// The mesh joins in `Runner::new()` before the graph exists, so this arrives
/// afterwards — the shape the hosted control plane's endpoint registry already
/// uses for something the runtime learns after it is on the mesh.
pub trait WhatThisRuntimeOffersOnTheMesh: Send + Sync {
    /// Every output port in this runtime's graph right now.
    fn output_ports_it_offers_right_now(&self) -> OutputPortsOfferedOnTheMesh;

    /// The channel the port at this address publishes to, and the sizing a
    /// subscriber on it must ask for — or `None` when no such port is wired.
    fn how_to_read_an_offered_output_port(
        &self,
        processor_display_name: &str,
        port_name: &str,
    ) -> Option<HowToReadAnOfferedOutputPort>;
}

/// What an egress needs to take a destination slot on an offered port's
/// channel: the iceoryx2 service name and the sizing the compiler opened it
/// with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HowToReadAnOfferedOutputPort {
    /// The channel data-service name the port publishes to.
    pub channel_service_name: String,
    /// The sizing every opener of that service must ask for.
    pub channel_sizing: crate::iceoryx2::ChannelSizing,
}

/// The seam through which the mesh reads this runtime's own graph, filled in
/// once the runtime has one.
///
/// Empty until then, which answers a peer with an empty list rather than
/// failing its query — a runtime still coming up genuinely offers nothing.
#[derive(Default)]
pub struct WhatThisRuntimeOffersOnTheMeshRegistry {
    reader: Mutex<Option<std::sync::Arc<dyn WhatThisRuntimeOffersOnTheMesh>>>,
}

impl WhatThisRuntimeOffersOnTheMeshRegistry {
    /// Record how the mesh reads this runtime's graph.
    pub fn record_how_to_read_this_runtimes_graph(
        &self,
        reader: std::sync::Arc<dyn WhatThisRuntimeOffersOnTheMesh>,
    ) {
        *self.reader.lock() = Some(reader);
    }

    /// Every output port this runtime offers right now.
    ///
    /// The reader is cloned out from under the lock before it is called: it
    /// reads the graph, which takes the lock a compile holds, and holding this
    /// one across that would queue every other caller behind a compile.
    pub fn output_ports_it_offers_right_now(&self) -> OutputPortsOfferedOnTheMesh {
        let reader = self.reader.lock().clone();
        reader
            .map(|reader| reader.output_ports_it_offers_right_now())
            .unwrap_or_default()
    }

    /// How to read one offered port's channel, or `None` while this runtime has
    /// no graph yet or no such port is wired.
    ///
    /// Cloned out from under the lock for the same reason
    /// [`Self::output_ports_it_offers_right_now`] is, and more so: this one
    /// opens a channel as well as reading the graph.
    pub fn how_to_read_an_offered_output_port(
        &self,
        processor_display_name: &str,
        port_name: &str,
    ) -> Option<HowToReadAnOfferedOutputPort> {
        let reader = self.reader.lock().clone();
        reader.and_then(|reader| {
            reader.how_to_read_an_offered_output_port(processor_display_name, port_name)
        })
    }
}

/// The queryable that answers what this runtime offers, and the thread that
/// answers it.
///
/// The callback only hands off. It runs on the link's receive loop, and reading
/// the graph takes the lock a compile holds — which waits for a helper to
/// report ready — so answering inline would stall every key arriving from that
/// peer and, past the lease, expire the link.
pub(super) struct OfferedOutputPortsQueryable {
    queryable: Option<zenoh::query::Queryable<()>>,
    answering_thread: Option<std::thread::JoinHandle<()>>,
}

impl OfferedOutputPortsQueryable {
    /// Declare the queryable and start the thread that answers it.
    pub(super) fn declare(
        session: &zenoh::Session,
        key_space: &RuntimeMeshKeySpace,
        this_runtimes_name: &str,
        offered: &std::sync::Arc<WhatThisRuntimeOffersOnTheMeshRegistry>,
    ) -> zenoh::Result<Self> {
        let answered_key = key_space.offered_output_ports_key_of(this_runtimes_name);
        let (asked, what_the_answering_thread_reads) =
            crossbeam_channel::unbounded::<zenoh::query::Query>();
        let queryable = session
            .declare_queryable(answered_key.clone())
            .callback(move |query| {
                let _ = asked.send(query);
            })
            .wait()?;

        let offered = std::sync::Arc::clone(offered);
        let answering_thread = std::thread::Builder::new()
            .name("streamlib-mesh-offered-ports".to_string())
            .spawn(move || {
                // Ends when the queryable is dropped, which drops the sender.
                while let Ok(query) = what_the_answering_thread_reads.recv() {
                    answer_one_query(&query, &answered_key, &offered);
                }
            })
            .map_err(|cannot_spawn| -> zenoh::Error { Box::new(cannot_spawn) })?;

        Ok(Self {
            queryable: Some(queryable),
            answering_thread: Some(answering_thread),
        })
    }
}

impl Drop for OfferedOutputPortsQueryable {
    fn drop(&mut self) {
        // The queryable first: dropping it drops the callback that holds the
        // answering thread's sender, which is what ends that thread.
        drop(self.queryable.take());
        if let Some(answering_thread) = self.answering_thread.take() {
            if answering_thread.join().is_err() {
                tracing::warn!(
                    "the thread answering what this runtime offers panicked; peers now read it \
                     as offering nothing"
                );
            }
        }
    }
}

/// Answer one peer with what this runtime offers at this moment.
fn answer_one_query(
    query: &zenoh::query::Query,
    answered_key: &str,
    offered: &WhatThisRuntimeOffersOnTheMeshRegistry,
) {
    match offered.output_ports_it_offers_right_now().encode() {
        Ok(wire_bytes) => {
            if let Err(reply_failure) = query.reply(answered_key, wire_bytes).wait() {
                tracing::debug!(
                    "a mesh peer asked what this runtime offers and the answer did not reach it: \
                     {reply_failure}"
                );
            }
        }
        Err(encode_failure) => {
            tracing::warn!(
                "this runtime could not list its output ports for a mesh peer: {encode_failure}"
            );
        }
    }
}

/// Ask `runtime_name` which output ports it offers, or `None` when it did not
/// answer in time or answered something this engine cannot read.
pub(super) fn ask_a_runtime_what_output_ports_it_offers(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    runtime_name: &str,
) -> Option<OutputPortsOfferedOnTheMesh> {
    let answers = session
        .get(key_space.offered_output_ports_key_of(runtime_name))
        .timeout(HOW_LONG_A_RUNTIME_HAS_TO_LIST_ITS_PORTS)
        .wait()
        .inspect_err(|query_failure| {
            tracing::debug!(
                "could not ask the mesh peer {runtime_name} what it offers: {query_failure}"
            );
        })
        .ok()?;
    for reply in answers {
        let Ok(answered) = reply.result() else {
            continue;
        };
        match OutputPortsOfferedOnTheMesh::decode(&answered.payload().to_bytes()) {
            Ok(listed) => return Some(listed),
            Err(unreadable) => {
                tracing::debug!(
                    "the mesh peer {runtime_name} listed its output ports in a form this engine \
                     cannot read: {unreadable}"
                );
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_listing() -> OutputPortsOfferedOnTheMesh {
        OutputPortsOfferedOnTheMesh {
            ports: vec![
                OutputPortOfferedOnTheMesh {
                    processor_display_name: "CameraSource".to_string(),
                    port_name: "video".to_string(),
                },
                OutputPortOfferedOnTheMesh {
                    processor_display_name: "MicrophoneSource".to_string(),
                    port_name: "audio".to_string(),
                },
            ],
        }
    }

    /// The document survives the wire whole.
    #[test]
    fn a_listing_round_trips_through_msgpack() {
        let encoded = a_listing().encode().expect("a listing encodes");
        assert_eq!(
            OutputPortsOfferedOnTheMesh::decode(&encoded).expect("it decodes"),
            a_listing()
        );
    }

    /// The keys are the contract, so they ride the wire as names rather than as
    /// positions a peer of another engine version would read wrong.
    #[test]
    fn the_wire_carries_the_field_names_rather_than_positions() {
        let encoded = a_listing().encode().expect("a listing encodes");
        assert_eq!(
            rmp_serde::from_slice::<serde_json::Value>(&encoded).expect("a msgpack map"),
            serde_json::json!({
                "ports": [
                    { "processor_display_name": "CameraSource", "port_name": "video" },
                    { "processor_display_name": "MicrophoneSource", "port_name": "audio" },
                ]
            })
        );
    }

    /// A port is offered only under its own display name and its own port name,
    /// never under half of each.
    #[test]
    fn a_port_is_offered_only_under_both_of_its_names() {
        let listed = a_listing();
        assert!(listed.offers("CameraSource", "video"));
        assert!(!listed.offers("CameraSource", "audio"));
        assert!(!listed.offers("MicrophoneSource", "video"));
        assert!(!listed.offers("NoSuchProcessor", "video"));
    }

    /// A refusal lists what is offered, sorted, so two runs read the same — and
    /// says so plainly when nothing is.
    #[test]
    fn a_refusal_lists_what_is_offered_in_a_stable_order() {
        assert_eq!(
            a_listing().listed_for_a_refusal(),
            "CameraSource/video, MicrophoneSource/audio"
        );
        assert_eq!(
            OutputPortsOfferedOnTheMesh::default().listed_for_a_refusal(),
            "nothing"
        );
    }

    /// A runtime that has no graph yet offers nothing rather than failing the
    /// query — which is what a runtime still coming up genuinely offers.
    #[test]
    fn a_registry_nobody_has_filled_offers_nothing() {
        let registry = WhatThisRuntimeOffersOnTheMeshRegistry::default();
        assert_eq!(
            registry.output_ports_it_offers_right_now(),
            OutputPortsOfferedOnTheMesh::default()
        );
        assert_eq!(
            registry.how_to_read_an_offered_output_port("CameraSource", "video"),
            None
        );
    }
}
