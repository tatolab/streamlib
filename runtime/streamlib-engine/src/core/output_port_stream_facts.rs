// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Output ports' stream facts: the facts a node's output ports carry now, and
//! the upstream facts its inbound links reach.
//!
//! Facts describe a stream. No read or write path consults them, `connect`
//! compares nothing against them, and no payload is ever read for them.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::descriptors::{
    ImageStreamFacts, PortFamily, PortStreamFacts, output_port_family_resolved_against_config,
    output_port_frame_rate_resolved_against_config,
};
use crate::core::error::{Error, Result};
use crate::core::graph::{LinkUniqueId, PortInfo, cast_exposed_name_to_url_safe};
use crate::iceoryx2::InboundLinkName;

#[derive(Debug)]
struct StreamFactsOfOneOutputPort {
    family: PortFamily,
    current_facts: Option<PortStreamFacts>,
}

/// The current stream facts of each of one node's output ports, shared by the
/// node, the processor's writer or its helper's bridge, and every downstream
/// node an inbound link reaches it from.
#[derive(Debug, Default)]
pub struct StreamFactsOfOutputPorts {
    facts_by_output_port: RwLock<HashMap<String, StreamFactsOfOneOutputPort>>,
}

impl StreamFactsOfOutputPorts {
    /// Each output port with its family resolved against `node_config`, an
    /// Image port carrying the frame rate it declares.
    pub fn as_declared_by(
        output_ports: &[PortInfo],
        node_config: Option<&serde_json::Value>,
    ) -> Self {
        Self {
            facts_by_output_port: RwLock::new(
                output_ports
                    .iter()
                    .map(|port| {
                        let family =
                            output_port_family_resolved_against_config(&port.family, node_config)
                                .unwrap_or(port.family.family);
                        let declared_frame_rate = output_port_frame_rate_resolved_against_config(
                            &port.family,
                            node_config,
                        )
                        .ok()
                        .flatten();
                        let current_facts = declared_frame_rate.map(|frame_rate| {
                            PortStreamFacts::Image(ImageStreamFacts {
                                frame_rate: Some(frame_rate),
                                ..ImageStreamFacts::default()
                            })
                        });
                        (
                            port.name.clone(),
                            StreamFactsOfOneOutputPort {
                                family,
                                current_facts,
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }

    /// The facts `output_port` carries now, or `None` — also for a port the
    /// node does not have.
    pub fn current_facts_of(&self, output_port: &str) -> Option<PortStreamFacts> {
        let output_port = cast_exposed_name_to_url_safe(output_port).ok()?;
        self.facts_by_output_port
            .read()
            .get(output_port.as_ref())
            .and_then(|port| port.current_facts.clone())
    }

    /// Replace `output_port`'s facts, refusing a port the node does not declare
    /// and facts of another family than the port's.
    pub fn replace_facts_of(&self, output_port: &str, facts: PortStreamFacts) -> Result<()> {
        let output_port = cast_exposed_name_to_url_safe(output_port)?;
        let mut facts_by_output_port = self.facts_by_output_port.write();
        let Some(port) = facts_by_output_port.get_mut(output_port.as_ref()) else {
            let mut declared_output_ports: Vec<&str> =
                facts_by_output_port.keys().map(String::as_str).collect();
            declared_output_ports.sort_unstable();
            return Err(Error::Link(format!(
                "no output port `{output_port}` to carry stream facts — the node declares {}",
                listed_or_none(&declared_output_ports)
            )));
        };
        crate::core::descriptors::refuse_stream_facts_of_another_family(
            &output_port,
            port.family,
            &facts,
        )?;
        port.current_facts = Some(facts);
        Ok(())
    }
}

fn listed_or_none(names: &[&str]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// The output port one inbound link reads, and the facts of the node that
/// holds it.
#[derive(Debug)]
struct UpstreamOutputPortOfAnInboundLink {
    link_id: LinkUniqueId,
    inbound_link_name: InboundLinkName,
    upstream_output_port: String,
    upstream_stream_facts: Arc<StreamFactsOfOutputPorts>,
}

/// For each of one node's input ports, the upstream output port of every
/// inbound link wired into it, in wiring order.
#[derive(Debug, Default)]
pub struct UpstreamStreamFactsByInboundLink {
    upstream_ports_by_input_port: Mutex<HashMap<String, Vec<UpstreamOutputPortOfAnInboundLink>>>,
}

impl UpstreamStreamFactsByInboundLink {
    /// Record link `link_id` into `input_port`, reading `upstream_output_port`
    /// of the node whose facts are `upstream_stream_facts`.
    pub fn record_inbound_link(
        &self,
        input_port: &str,
        link_id: &LinkUniqueId,
        inbound_link_name: InboundLinkName,
        upstream_output_port: &str,
        upstream_stream_facts: Arc<StreamFactsOfOutputPorts>,
    ) {
        let mut upstream_ports_by_input_port = self.upstream_ports_by_input_port.lock();
        let upstream_ports = upstream_ports_by_input_port
            .entry(input_port.to_string())
            .or_default();
        upstream_ports.retain(|upstream_port| &upstream_port.link_id != link_id);
        upstream_ports.push(UpstreamOutputPortOfAnInboundLink {
            link_id: link_id.clone(),
            inbound_link_name,
            upstream_output_port: upstream_output_port.to_string(),
            upstream_stream_facts,
        });
    }

    /// Forget link `link_id`, wherever it was wired.
    pub fn forget_inbound_link(&self, link_id: &LinkUniqueId) {
        let mut upstream_ports_by_input_port = self.upstream_ports_by_input_port.lock();
        for upstream_ports in upstream_ports_by_input_port.values_mut() {
            upstream_ports.retain(|upstream_port| &upstream_port.link_id != link_id);
        }
        upstream_ports_by_input_port.retain(|_, upstream_ports| !upstream_ports.is_empty());
    }

    /// The current facts of the output port feeding `input_port`, or `None`
    /// when nothing feeds it or the upstream port states none.
    ///
    /// A port fed by more than one link is asked about one of them by its
    /// `inbound_link` name; asked about none, it is refused naming them.
    pub fn stream_facts_of_inbound_link(
        &self,
        input_port: &str,
        inbound_link: Option<&str>,
    ) -> Result<Option<PortStreamFacts>> {
        let input_port = cast_exposed_name_to_url_safe(input_port)?;
        let upstream_ports_by_input_port = self.upstream_ports_by_input_port.lock();
        let upstream_ports = upstream_ports_by_input_port
            .get(input_port.as_ref())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let inbound_link_names = || {
            listed_or_none(
                &upstream_ports
                    .iter()
                    .map(|upstream_port| upstream_port.inbound_link_name.as_str())
                    .collect::<Vec<_>>(),
            )
        };
        let upstream_port = match (inbound_link, upstream_ports) {
            (None, []) => return Ok(None),
            (None, [only_upstream_port]) => only_upstream_port,
            (None, _) => {
                return Err(Error::Link(format!(
                    "input port `{input_port}` is fed by {} links — name the one whose stream \
                     facts to read: {}",
                    upstream_ports.len(),
                    inbound_link_names()
                )));
            }
            (Some(inbound_link), _) => upstream_ports
                .iter()
                .find(|upstream_port| upstream_port.inbound_link_name.as_str() == inbound_link)
                .ok_or_else(|| {
                    Error::Link(format!(
                        "input port `{input_port}` has no inbound link `{inbound_link}` — its \
                         links are: {}",
                        inbound_link_names()
                    ))
                })?,
        };
        Ok(upstream_port
            .upstream_stream_facts
            .current_facts_of(&upstream_port.upstream_output_port))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::descriptors::{
        ImageOutputPortFrameRate, ImageStreamFrameRate, PacketsStreamFacts, PortFamilyDeclaration,
    };

    fn output_port(name: &str, family: PortFamilyDeclaration) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            description: String::new(),
            port_kind: Default::default(),
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            pin_default: None,
            family,
        }
    }

    fn packets_port(name: &str) -> PortInfo {
        output_port(name, PortFamilyDeclaration::of_family(PortFamily::Packets))
    }

    fn opus_facts() -> PortStreamFacts {
        PacketsStreamFacts {
            rfc6381_codec: Some("opus".into()),
            sample_rate: Some(48_000),
            channel_count: Some(2),
            ..PacketsStreamFacts::default()
        }
        .into()
    }

    #[test]
    fn an_image_port_carries_the_frame_rate_its_declaration_reads_from_config() {
        let camera_video = output_port(
            "video",
            PortFamilyDeclaration {
                frame_rate: Some(ImageOutputPortFrameRate::FromConfig {
                    config_key: "frames_per_second".into(),
                }),
                ..PortFamilyDeclaration::of_family(PortFamily::Image)
            },
        );
        let facts = StreamFactsOfOutputPorts::as_declared_by(
            &[camera_video.clone(), packets_port("encoded")],
            Some(&json!({"frames_per_second": 30})),
        );
        assert_eq!(
            facts.current_facts_of("video"),
            Some(PortStreamFacts::Image(ImageStreamFacts {
                frame_rate: Some(ImageStreamFrameRate {
                    numerator: 30,
                    denominator: 1
                }),
                ..ImageStreamFacts::default()
            }))
        );
        assert_eq!(facts.current_facts_of("encoded"), None);
        assert_eq!(
            StreamFactsOfOutputPorts::as_declared_by(&[camera_video], None)
                .current_facts_of("video"),
            None
        );
    }

    #[test]
    fn replaced_facts_are_the_ones_read_and_another_familys_are_refused_leaving_the_last() {
        let facts = StreamFactsOfOutputPorts::as_declared_by(&[packets_port("encoded")], None);
        facts
            .replace_facts_of("encoded", opus_facts())
            .expect("a Packets port carries Packets facts");
        assert_eq!(facts.current_facts_of("encoded"), Some(opus_facts()));

        let refusal = facts
            .replace_facts_of("encoded", ImageStreamFacts::default().into())
            .expect_err("a Packets port carries no Image facts")
            .to_string();
        assert!(
            refusal.contains("encoded") && refusal.contains("Packets") && refusal.contains("Image"),
            "got {refusal}"
        );
        assert_eq!(facts.current_facts_of("encoded"), Some(opus_facts()));
    }

    #[test]
    fn a_port_whose_family_comes_from_config_takes_the_facts_of_that_family() {
        let received = output_port(
            "media",
            PortFamilyDeclaration {
                family_from_config_key: Some("family".into()),
                ..PortFamilyDeclaration::of_family(PortFamily::Records)
            },
        );
        let facts = StreamFactsOfOutputPorts::as_declared_by(
            &[received],
            Some(&json!({"family": "packets"})),
        );
        facts
            .replace_facts_of("media", opus_facts())
            .expect("the config made the port Packets");
    }

    #[test]
    fn facts_for_a_port_the_node_does_not_declare_are_refused_naming_the_ports_it_does() {
        let facts = StreamFactsOfOutputPorts::as_declared_by(&[packets_port("encoded")], None);
        let refusal = facts
            .replace_facts_of("audio", opus_facts())
            .expect_err("a port the node does not declare carries no facts")
            .to_string();
        assert!(
            refusal.contains("`audio`") && refusal.contains("encoded"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_reader_reads_the_current_facts_of_the_port_its_inbound_link_reads() {
        let upstream = Arc::new(StreamFactsOfOutputPorts::as_declared_by(
            &[packets_port("encoded")],
            None,
        ));
        let inbound = UpstreamStreamFactsByInboundLink::default();
        assert_eq!(
            inbound
                .stream_facts_of_inbound_link("encoded_in", None)
                .expect("an unfed port answers"),
            None
        );

        inbound.record_inbound_link(
            "encoded_in",
            &LinkUniqueId::from("link-1"),
            InboundLinkName::from("encoder/encoded"),
            "encoded",
            Arc::clone(&upstream),
        );
        assert_eq!(
            inbound
                .stream_facts_of_inbound_link("encoded_in", None)
                .expect("a port fed by one link names it implicitly"),
            None
        );
        upstream
            .replace_facts_of("encoded", opus_facts())
            .expect("the upstream states its facts");
        assert_eq!(
            inbound
                .stream_facts_of_inbound_link("encoded_in", Some("encoder/encoded"))
                .expect("a named link answers"),
            Some(opus_facts()),
            "the reader sees the upstream's current facts, not the ones at wiring"
        );

        inbound.forget_inbound_link(&LinkUniqueId::from("link-1"));
        assert_eq!(
            inbound
                .stream_facts_of_inbound_link("encoded_in", None)
                .expect("a port whose link went answers"),
            None
        );
    }

    #[test]
    fn a_port_fed_by_two_links_is_asked_about_one_by_name() {
        let first = Arc::new(StreamFactsOfOutputPorts::as_declared_by(
            &[packets_port("encoded")],
            None,
        ));
        first
            .replace_facts_of("encoded", opus_facts())
            .expect("the first upstream states its facts");
        let second = Arc::new(StreamFactsOfOutputPorts::as_declared_by(
            &[packets_port("encoded")],
            None,
        ));
        let inbound = UpstreamStreamFactsByInboundLink::default();
        inbound.record_inbound_link(
            "tracks",
            &LinkUniqueId::from("link-1"),
            InboundLinkName::from("first/encoded"),
            "encoded",
            first,
        );
        inbound.record_inbound_link(
            "tracks",
            &LinkUniqueId::from("link-2"),
            InboundLinkName::from("second/encoded"),
            "encoded",
            second,
        );

        let refusal = inbound
            .stream_facts_of_inbound_link("tracks", None)
            .expect_err("two links leave the facts ambiguous")
            .to_string();
        assert!(
            refusal.contains("first/encoded") && refusal.contains("second/encoded"),
            "the refusal must name both links; got {refusal}"
        );
        assert_eq!(
            inbound
                .stream_facts_of_inbound_link("tracks", Some("first/encoded"))
                .expect("the first link answers"),
            Some(opus_facts())
        );
        assert_eq!(
            inbound
                .stream_facts_of_inbound_link("tracks", Some("second/encoded"))
                .expect("the second link answers"),
            None
        );
        inbound
            .stream_facts_of_inbound_link("tracks", Some("third/encoded"))
            .expect_err("a link that does not feed the port is refused");
    }
}
