// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which of a runtime's output ports a peer may pull, and how it asks.
//!
//! Answered at query time and never announced: a graph changes while it runs,
//! and a list put on the mesh once would be a list of what used to be there.
//!
//! A runtime offers only what it can send. A port it holds and cannot send is
//! answered beside the offer with the reason, not left out silently: a reader
//! refused as though the port did not exist would go looking for a port that is
//! right there, and a reader told nothing at all would wait on an egress that
//! can never start.
//!
//! A port it offers and stopped sending is answered the same way, under its own
//! key: the port is on offer and a later reader still revives it, so the answer
//! is not a refusal — but the reason its last egress ended lives only in this
//! runtime's log until it rides this document, and a reader two machines away
//! cannot read that log.
//!
//! The wire is msgpack, the same codec every bag rides, and the field names are
//! the contract — a peer of another engine version reads this document.

use std::collections::BTreeMap;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use zenoh::Wait;

use crate::core::runtime::mesh::runtime_mesh_key::{ReaderOfAnOutputPort, RuntimeMeshKeySpace};

/// How long a runtime has to say which ports it offers before the asking side
/// gives up and asks again on its next pass. Engine-chosen; nothing authorable.
const HOW_LONG_A_RUNTIME_HAS_TO_LIST_ITS_PORTS: Duration = Duration::from_secs(2);

/// How many unanswered offered-ports queries this runtime holds before it drops
/// one. Deep enough that every runtime on a mesh may ask at once while a compile
/// holds the graph lock; shallow enough that a peer asking in a loop cannot
/// grow this runtime's memory. Engine-chosen; nothing authorable.
const HOW_MANY_UNANSWERED_QUERIES_ARE_HELD: usize = 64;

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

/// One output port a runtime holds and cannot send, and why.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OutputPortThisRuntimeHoldsAndCannotSend {
    /// The display name of the processor that owns the port.
    pub processor_display_name: String,
    /// The port's own name.
    pub port_name: String,
    /// Why the mesh cannot send it, in the holding runtime's own words — what a
    /// reader's refusal quotes, so the reason never lives only in this
    /// runtime's log.
    pub why_it_cannot_be_sent: String,
}

/// One output port a runtime offers and stopped sending, and why its last
/// egress ended.
///
/// Separate from [`OutputPortThisRuntimeHoldsAndCannotSend`] because the two
/// read differently at the reader: that one is a refusal, and this port is
/// still on offer — the next runtime to begin reading it starts a fresh egress.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OutputPortThisRuntimeStoppedSending {
    /// The display name of the processor that owns the port.
    pub processor_display_name: String,
    /// The port's own name.
    pub port_name: String,
    /// Why this runtime stopped sending it, in its own words — what a waiting
    /// reader's link names, so the reason never lives only in this runtime's
    /// log.
    pub why_it_stopped_being_sent: String,
}

/// The document a runtime answers with when a peer asks what it offers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutputPortsOfferedOnTheMesh {
    /// Every output port in the runtime's graph that it can send, at the moment
    /// it was asked.
    pub ports: Vec<OutputPortOfferedOnTheMesh>,
    /// Every output port in that graph it cannot send, each with the reason.
    ///
    /// Absent reads as empty, because that is what it means: a peer that names
    /// no unsendable ports holds none this reader can be told about. It is what
    /// a build predating this key answers, and the version gate does not
    /// separate those — it compares crate versions, and a released wheel and a
    /// local build of the same version both pass it. Without this the whole
    /// document would fail to decode, and *every* link from that peer — the ones
    /// it can serve included — would wait on a runtime that had in fact
    /// answered.
    #[serde(default)]
    pub ports_it_holds_and_cannot_send: Vec<OutputPortThisRuntimeHoldsAndCannotSend>,
    /// Every port it offers whose last egress ended, each with the reason.
    ///
    /// Absent reads as empty for the reason the key above states, and a port
    /// here is still listed under `ports`: it is offered, and a runtime that
    /// begins reading it starts a fresh egress.
    #[serde(default)]
    pub ports_it_stopped_sending: Vec<OutputPortThisRuntimeStoppedSending>,
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

    /// Why the runtime cannot send `port_name` on `processor_display_name`, for
    /// a port it holds and said it cannot send — `None` for every other port,
    /// offered or absent.
    pub fn why_it_cannot_send(
        &self,
        processor_display_name: &str,
        port_name: &str,
    ) -> Option<&str> {
        self.ports_it_holds_and_cannot_send
            .iter()
            .find(|held| {
                held.processor_display_name == processor_display_name && held.port_name == port_name
            })
            .map(|held| held.why_it_cannot_be_sent.as_str())
    }

    /// Why the runtime stopped sending `port_name` on `processor_display_name`,
    /// for a port it offers and said its last egress ended — `None` for every
    /// other port.
    pub fn why_it_stopped_being_sent(
        &self,
        processor_display_name: &str,
        port_name: &str,
    ) -> Option<&str> {
        self.ports_it_stopped_sending
            .iter()
            .find(|stopped| {
                stopped.processor_display_name == processor_display_name
                    && stopped.port_name == port_name
            })
            .map(|stopped| stopped.why_it_stopped_being_sent.as_str())
    }

    /// Every offered port, rendered for a refusal that has to say what *is*
    /// offered rather than only what is not.
    ///
    /// A port the runtime holds and cannot send is not one of them: naming it
    /// here would answer a reader looking for somewhere to point its link with
    /// a port that would refuse it the same way.
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
    /// Every output port in this runtime's graph right now, split into the ones
    /// it can send and the ones it holds and cannot.
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
/// channel: the iceoryx2 service name, the sizing the compiler opened it with,
/// and — for a port whose processor runs in a helper — that helper's answer
/// that it opened the publisher this channel carries from.
#[derive(Debug, Clone)]
pub struct HowToReadAnOfferedOutputPort {
    /// The channel data-service name the port publishes to.
    pub channel_service_name: String,
    /// The sizing every opener of that service must ask for.
    pub channel_sizing: crate::iceoryx2::ChannelSizing,
    /// The helper's answer that it opened its publisher, `None` for a port
    /// whose processor runs in this process and for one already publishing.
    /// An egress waits on it before saying the port is being sent, so a reader
    /// never reads `wired` over a helper that refused.
    pub the_helpers_answer_that_it_opened_its_publisher:
        Option<std::sync::Arc<crate::core::processors::OutOfProcessLinkWireReply>>,
}

/// The seam through which the mesh reads this runtime's own graph, filled in
/// once the runtime has one.
///
/// Empty until then, which answers a peer with an empty list rather than
/// failing its query — a runtime still coming up genuinely offers nothing.
#[derive(Default)]
pub struct WhatThisRuntimeOffersOnTheMeshRegistry {
    reader: Mutex<Option<std::sync::Arc<dyn WhatThisRuntimeOffersOnTheMesh>>>,
    /// Why each port's last egress ended, written by the egress table and read
    /// into every answer.
    ///
    /// Here rather than on the graph reader because a graph knows nothing of
    /// egresses, and this is already the seam between what the runtime has and
    /// what the mesh says about it.
    why_each_port_stopped_being_sent: Mutex<BTreeMap<OutputPortOfferedOnTheMesh, String>>,
}

impl WhatThisRuntimeOffersOnTheMeshRegistry {
    /// Record how the mesh reads this runtime's graph.
    pub fn record_how_to_read_this_runtimes_graph(
        &self,
        reader: std::sync::Arc<dyn WhatThisRuntimeOffersOnTheMesh>,
    ) {
        *self.reader.lock() = Some(reader);
    }

    /// Record why this runtime stopped sending `port`.
    pub fn record_why_it_stopped_sending_an_output_port(
        &self,
        port: OutputPortOfferedOnTheMesh,
        why_it_stopped_being_sent: String,
    ) {
        self.why_each_port_stopped_being_sent
            .lock()
            .insert(port, why_it_stopped_being_sent);
    }

    /// Forget that this runtime stopped sending `port`, because something is
    /// sending it again or nobody is asking for it any more.
    pub fn forget_that_it_stopped_sending_an_output_port(&self, port: &OutputPortOfferedOnTheMesh) {
        self.why_each_port_stopped_being_sent.lock().remove(port);
    }

    /// Every output port this runtime offers right now.
    ///
    /// The reader is cloned out from under the lock before it is called: it
    /// reads the graph, which takes the lock a compile holds, and holding this
    /// one across that would queue every other caller behind a compile.
    pub fn output_ports_it_offers_right_now(&self) -> OutputPortsOfferedOnTheMesh {
        let reader = self.reader.lock().clone();
        let mut offered = reader
            .map(|reader| reader.output_ports_it_offers_right_now())
            .unwrap_or_default();
        // Only ports the graph still offers: a record for a port whose
        // processor has since been removed would answer a reader about a port
        // this runtime no longer has, where the missing-port refusal is the
        // true answer.
        offered.ports_it_stopped_sending = self
            .why_each_port_stopped_being_sent
            .lock()
            .iter()
            .filter(|(port, _)| offered.offers(&port.processor_display_name, &port.port_name))
            .map(
                |(port, why_it_stopped_being_sent)| OutputPortThisRuntimeStoppedSending {
                    processor_display_name: port.processor_display_name.clone(),
                    port_name: port.port_name.clone(),
                    why_it_stopped_being_sent: why_it_stopped_being_sent.clone(),
                },
            )
            .collect();
        offered
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
        // Bounded, and a query that will not fit is dropped rather than queued:
        // any peer that knows this runtime's name can ask, one thread answers
        // them in turn, and that thread can be waiting on the lock a compile
        // holds. An unbounded queue would grow as fast as a peer cared to ask.
        // A dropped query is a query with no reply, which the asking side
        // already reads as its own timeout and retries on its next pass.
        let (asked, what_the_answering_thread_reads) =
            crossbeam_channel::bounded::<zenoh::query::Query>(HOW_MANY_UNANSWERED_QUERIES_ARE_HELD);
        let queryable = session
            .declare_queryable(answered_key.clone())
            .callback(move |query| {
                if asked.try_send(query).is_err() {
                    tracing::warn!(
                        "this runtime is being asked what it offers faster than it can answer;                          the asking runtime reads no reply as a timeout and asks again"
                    );
                }
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

/// A graph reader that answers whatever it is handed, so a test can state what
/// the graph says and read what the mesh answers.
#[cfg(test)]
struct AGraphOfferingExactly(OutputPortsOfferedOnTheMesh);

#[cfg(test)]
impl WhatThisRuntimeOffersOnTheMesh for AGraphOfferingExactly {
    fn output_ports_it_offers_right_now(&self) -> OutputPortsOfferedOnTheMesh {
        self.0.clone()
    }

    fn how_to_read_an_offered_output_port(
        &self,
        _processor_display_name: &str,
        _port_name: &str,
    ) -> Option<HowToReadAnOfferedOutputPort> {
        None
    }
}

/// A registry whose graph offers exactly `ports` and nothing else.
///
/// Here rather than in this module's own tests because the egress table reads
/// its record back through this same door, and two fixtures standing up one
/// registry are two chances to prove different things by accident.
#[cfg(test)]
pub(crate) fn a_registry_whose_graph_offers(
    ports: &[(&str, &str)],
) -> WhatThisRuntimeOffersOnTheMeshRegistry {
    let registry = WhatThisRuntimeOffersOnTheMeshRegistry::default();
    registry.record_how_to_read_this_runtimes_graph(std::sync::Arc::new(AGraphOfferingExactly(
        OutputPortsOfferedOnTheMesh {
            ports: ports
                .iter()
                .map(|(processor_display_name, port_name)| OutputPortOfferedOnTheMesh {
                    processor_display_name: processor_display_name.to_string(),
                    port_name: port_name.to_string(),
                })
                .collect(),
            ..Default::default()
        },
    )));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_port(processor_display_name: &str, port_name: &str) -> OutputPortOfferedOnTheMesh {
        OutputPortOfferedOnTheMesh {
            processor_display_name: processor_display_name.to_string(),
            port_name: port_name.to_string(),
        }
    }

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
            ports_it_holds_and_cannot_send: vec![OutputPortThisRuntimeHoldsAndCannotSend {
                processor_display_name: "CameraSource".to_string(),
                port_name: "depthOut".to_string(),
                why_it_cannot_be_sent: "its channel cannot be named: it contains 'O'".to_string(),
            }],
            ports_it_stopped_sending: vec![OutputPortThisRuntimeStoppedSending {
                processor_display_name: "MicrophoneSource".to_string(),
                port_name: "audio".to_string(),
                why_it_stopped_being_sent: "it could not take a destination slot".to_string(),
            }],
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
                ],
                "ports_it_holds_and_cannot_send": [
                    {
                        "processor_display_name": "CameraSource",
                        "port_name": "depthOut",
                        "why_it_cannot_be_sent": "its channel cannot be named: it contains 'O'",
                    },
                ],
                "ports_it_stopped_sending": [
                    {
                        "processor_display_name": "MicrophoneSource",
                        "port_name": "audio",
                        "why_it_stopped_being_sent": "it could not take a destination slot",
                    },
                ],
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

    /// A document from a peer that names no unsendable ports at all — the shape
    /// a build predating that key answers — reads as one holding none, rather
    /// than failing to decode and stranding every link from that peer.
    #[test]
    fn a_document_naming_no_unsendable_ports_reads_as_holding_none() {
        let without_the_key = rmp_serde::to_vec_named(&serde_json::json!({
            "ports": [{ "processor_display_name": "CameraSource", "port_name": "video" }],
        }))
        .expect("the older shape encodes");

        let listed = OutputPortsOfferedOnTheMesh::decode(&without_the_key)
            .expect("a document with no unsendable ports still decodes");
        assert!(listed.offers("CameraSource", "video"));
        assert!(listed.ports_it_holds_and_cannot_send.is_empty());
        assert!(listed.ports_it_stopped_sending.is_empty());
    }

    /// A port whose last egress ended stays on offer and answers why under both
    /// of its own names.
    ///
    /// What it catches: folding this into `ports_it_holds_and_cannot_send`,
    /// which a reader reads as a refusal — the link would go final and no later
    /// reader could revive the port.
    #[test]
    fn a_port_that_stopped_being_sent_is_still_offered_and_answers_why() {
        let listed = a_listing();
        assert!(
            listed.offers("MicrophoneSource", "audio"),
            "a port whose egress ended is still one a later reader revives"
        );
        assert_eq!(
            listed.why_it_cannot_send("MicrophoneSource", "audio"),
            None,
            "a port that stopped being sent is not one this runtime refuses"
        );
        assert_eq!(
            listed.why_it_stopped_being_sent("MicrophoneSource", "audio"),
            Some("it could not take a destination slot")
        );
        assert_eq!(
            listed.why_it_stopped_being_sent("CameraSource", "video"),
            None
        );
        assert_eq!(
            listed.why_it_stopped_being_sent("MicrophoneSource", "video"),
            None
        );
    }

    /// What the egress table records reaches the answer a peer reads, and
    /// something sending the port again takes it back out.
    #[test]
    fn what_stopped_being_sent_rides_the_answer_until_it_is_forgotten() {
        let registry = a_registry_whose_graph_offers(&[("CameraSource", "video")]);
        assert!(
            registry
                .output_ports_it_offers_right_now()
                .ports_it_stopped_sending
                .is_empty()
        );

        registry.record_why_it_stopped_sending_an_output_port(
            a_port("CameraSource", "video"),
            "its publisher did not declare".to_string(),
        );
        assert_eq!(
            registry
                .output_ports_it_offers_right_now()
                .why_it_stopped_being_sent("CameraSource", "video"),
            Some("its publisher did not declare")
        );

        registry.forget_that_it_stopped_sending_an_output_port(&a_port("CameraSource", "video"));
        assert!(
            registry
                .output_ports_it_offers_right_now()
                .ports_it_stopped_sending
                .is_empty()
        );
    }

    /// A record for a port the graph no longer holds is not answered.
    ///
    /// What it catches: a processor removed while its egress was failing would
    /// otherwise have this runtime answer about a port it does not have, where
    /// the missing-port refusal — which names what *is* offered — is the true
    /// answer.
    #[test]
    fn a_record_for_a_port_the_graph_no_longer_holds_is_not_answered() {
        let registry = a_registry_whose_graph_offers(&[]);
        registry.record_why_it_stopped_sending_an_output_port(
            a_port("CameraSource", "video"),
            "its publisher did not declare".to_string(),
        );

        assert!(
            registry
                .output_ports_it_offers_right_now()
                .ports_it_stopped_sending
                .is_empty()
        );
    }

    /// A port the runtime holds and cannot send is not offered, and answers the
    /// reason under both of its own names — which is what a reader's refusal
    /// quotes instead of saying the port does not exist.
    #[test]
    fn a_port_held_and_unsendable_is_not_offered_and_answers_why() {
        let listed = a_listing();
        assert!(
            !listed.offers("CameraSource", "depthOut"),
            "a port that cannot be sent is not on offer"
        );
        assert_eq!(
            listed.why_it_cannot_send("CameraSource", "depthOut"),
            Some("its channel cannot be named: it contains 'O'")
        );
        assert_eq!(listed.why_it_cannot_send("CameraSource", "video"), None);
        assert_eq!(
            listed.why_it_cannot_send("CameraSource", "no_such_port"),
            None
        );
        assert_eq!(
            listed.why_it_cannot_send("NoSuchProcessor", "depthOut"),
            None
        );
    }

    /// A refusal lists what is offered, sorted, so two runs read the same — and
    /// says so plainly when nothing is. A port held and unsendable is never one
    /// of them: it would refuse the reader that followed it the same way.
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
        let holding_only_what_it_cannot_send = OutputPortsOfferedOnTheMesh {
            ports: vec![],
            ..a_listing()
        };
        assert_eq!(
            holding_only_what_it_cannot_send.listed_for_a_refusal(),
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
        let how_to_read = registry.how_to_read_an_offered_output_port("CameraSource", "video");
        assert!(how_to_read.is_none(), "{how_to_read:?}");
    }
}
