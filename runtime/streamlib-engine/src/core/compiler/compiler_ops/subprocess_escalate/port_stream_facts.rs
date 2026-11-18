// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A helper's stream-facts ops: replacing an output port's facts, and reading
//! the facts of the output port feeding one of its inputs. Neither reads a
//! payload.

use std::sync::Arc;

use super::failure_the_node_reported_of_itself::FailureTheNodeReportedOfItself;
use super::node_not_ready_reports::NodeNotReadyReports;
use super::process_raising_the_node_reports::ProcessRaisingTheNodeReports;
use super::shared_native_service_lease::SharedNativeServiceLeasesOfOneHelper;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestReadInboundLinkStreamFacts, EscalateRequestSetOutputPortStreamFacts,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::texture_pools_and_rings::node_output_surface_pool::OutputFramesKeptByTheOutputLinksOfOneNode;
use crate::core::graph::HowOneReadersHelperRanEachFunctionNode;
use crate::core::output_port_stream_facts::{
    StreamFactsOfOutputPorts, UpstreamStreamFactsByInboundLink,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct WhatAHelperPlacedNodeReaches {
    pub(crate) stream_facts_of_its_output_ports: Arc<StreamFactsOfOutputPorts>,
    pub(crate) upstream_stream_facts_by_its_inbound_links: Arc<UpstreamStreamFactsByInboundLink>,
    pub(crate) shared_native_service_leases: Arc<SharedNativeServiceLeasesOfOneHelper>,
    pub(crate) failure_the_node_reported_of_itself: Arc<FailureTheNodeReportedOfItself>,
    pub(crate) process_raising_the_node_reports: Arc<ProcessRaisingTheNodeReports>,
    pub(crate) how_its_islands_ran_each_function_node: Arc<HowOneReadersHelperRanEachFunctionNode>,
    pub(crate) node_not_ready_reports: Arc<NodeNotReadyReports>,
    pub(crate) output_frames_kept_by_its_output_links:
        Arc<OutputFramesKeptByTheOutputLinksOfOneNode>,
    /// The node's own keys of its project's store; `None` for a helper no node's context spawned.
    pub(crate) store_of_the_node: Option<crate::core::context::StoreOfOneNodeOpenedOnFirstUse>,
}

/// Replace the stream facts the helper's output port carries.
pub(super) fn handle_set_output_port_stream_facts(
    what_the_node_reaches: &WhatAHelperPlacedNodeReaches,
    rid: String,
    request: EscalateRequestSetOutputPortStreamFacts,
) -> EscalateResponse {
    let EscalateRequestSetOutputPortStreamFacts {
        request_id: _,
        port,
        stream_facts,
    } = request;
    match what_the_node_reaches
        .stream_facts_of_its_output_ports
        .replace_facts_of(&port, stream_facts)
    {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            ..Default::default()
        }),
        Err(refusal) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: refusal.to_string(),
        }),
    }
}

/// Answer with the current facts of the output port feeding the helper's input
/// port, or with none.
pub(super) fn handle_read_inbound_link_stream_facts(
    what_the_node_reaches: &WhatAHelperPlacedNodeReaches,
    rid: String,
    request: EscalateRequestReadInboundLinkStreamFacts,
) -> EscalateResponse {
    let EscalateRequestReadInboundLinkStreamFacts {
        request_id: _,
        port,
        inbound_link,
    } = request;
    match what_the_node_reaches
        .upstream_stream_facts_by_its_inbound_links
        .stream_facts_of_inbound_link(&port, inbound_link.as_deref())
    {
        Ok(stream_facts) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            stream_facts,
            ..Default::default()
        }),
        Err(refusal) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: refusal.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::descriptors::{PacketsStreamFacts, PortFamily, PortStreamFacts};
    use crate::core::graph::PortInfo;
    use crate::iceoryx2::InboundLinkName;

    fn output_port_named(name: &str) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            description: String::new(),
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            pin_default: None,
            pin: None,
            lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
            former_names: Vec::new(),
            may_be_unset: false,
            family: crate::core::descriptors::PortFamilyDeclaration::of_family(PortFamily::Packets),
            input_mode: None,
            range: None,
            meaning: None,
        }
    }

    fn dispatched(
        what_the_node_reaches: &WhatAHelperPlacedNodeReaches,
        request: serde_json::Value,
    ) -> EscalateResponse {
        match serde_json::from_value::<EscalateRequest>(request)
            .expect("the request decodes from its wire form")
        {
            EscalateRequest::SetOutputPortStreamFacts(request) => {
                handle_set_output_port_stream_facts(
                    what_the_node_reaches,
                    request.request_id.clone(),
                    request,
                )
            }
            EscalateRequest::ReadInboundLinkStreamFacts(request) => {
                handle_read_inbound_link_stream_facts(
                    what_the_node_reaches,
                    request.request_id.clone(),
                    request,
                )
            }
            other => panic!("not a stream-facts op: {other:?}"),
        }
    }

    #[test]
    fn a_helpers_set_replaces_its_ports_facts_and_a_downstream_read_sees_them() {
        let what_the_writer_reaches = WhatAHelperPlacedNodeReaches {
            stream_facts_of_its_output_ports: Arc::new(StreamFactsOfOutputPorts::as_declared_by(
                &[output_port_named("encoded_video")],
                None,
            )),
            ..Default::default()
        };
        let what_the_reader_reaches = WhatAHelperPlacedNodeReaches::default();
        what_the_reader_reaches
            .upstream_stream_facts_by_its_inbound_links
            .record_inbound_link(
                "video_in",
                &crate::core::graph::LinkUniqueId::from("link-1"),
                InboundLinkName::from("encoder/encoded_video"),
                "encoded_video",
                Arc::clone(&what_the_writer_reaches.stream_facts_of_its_output_ports),
            );

        let set = dispatched(
            &what_the_writer_reaches,
            json!({
                "op": "set_output_port_stream_facts",
                "request_id": "set-1",
                "port": "encoded_video",
                "stream_facts": {"family": "packets", "rfc6381_codec": "avc1.64001f", "coded_width": 1280, "coded_height": 720},
            }),
        );
        assert!(matches!(set, EscalateResponse::Ok(_)), "got {set:?}");

        let read = dispatched(
            &what_the_reader_reaches,
            json!({
                "op": "read_inbound_link_stream_facts",
                "request_id": "read-1",
                "port": "video_in",
            }),
        );
        let EscalateResponse::Ok(answer) = read else {
            panic!("the read is answered ok; got {read:?}");
        };
        assert_eq!(
            answer.stream_facts,
            Some(PortStreamFacts::Packets(PacketsStreamFacts {
                rfc6381_codec: Some("avc1.64001f".into()),
                coded_width: Some(1280),
                coded_height: Some(720),
                ..PacketsStreamFacts::default()
            }))
        );
    }

    #[test]
    fn a_set_of_another_familys_facts_is_refused_naming_the_port_and_both_families() {
        let what_the_node_reaches = WhatAHelperPlacedNodeReaches {
            stream_facts_of_its_output_ports: Arc::new(StreamFactsOfOutputPorts::as_declared_by(
                &[output_port_named("encoded")],
                None,
            )),
            ..Default::default()
        };
        let refused = dispatched(
            &what_the_node_reaches,
            json!({
                "op": "set_output_port_stream_facts",
                "request_id": "set-2",
                "port": "encoded",
                "stream_facts": {"family": "image", "frame_rate": {"numerator": 30, "denominator": 1}},
            }),
        );
        let EscalateResponse::Err(refusal) = refused else {
            panic!("another family's facts are refused; got {refused:?}");
        };
        assert_eq!(refusal.request_id, "set-2");
        assert!(
            refusal.message.contains("encoded")
                && refusal.message.contains("Packets")
                && refusal.message.contains("Image"),
            "got {}",
            refusal.message
        );
        assert_eq!(
            what_the_node_reaches
                .stream_facts_of_its_output_ports
                .current_facts_of("encoded"),
            None
        );
    }
}
