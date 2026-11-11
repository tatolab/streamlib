// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A helper's stream-facts ops: replacing an output port's facts, and reading
//! the facts of the output port feeding one of its inputs. Neither reads a
//! payload.

use std::sync::Arc;

use super::failure_the_node_reported_of_itself::FailureTheNodeReportedOfItself;
use super::process_raising_the_node_reports::ProcessRaisingTheNodeReports;
use super::shared_native_service_lease::SharedNativeServiceLeasesOfOneHelper;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestReadInboundLinkStreamFacts, EscalateRequestSetOutputPortStreamFacts,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::processor_output_surface_pool::OutputFramesKeptByTheOutputLinksOfOneProcessor;
use crate::core::output_port_stream_facts::{
    StreamFactsOfOutputPorts, UpstreamStreamFactsByInboundLink,
};

/// What the node a helper-placed processor runs for reaches beside the GPU: its
/// own output ports' stream facts, the upstream ones its inbound links
/// reach, the helper's leases on the runtime's shared native services, the
/// failure the node reported of itself, what its `process()` raises, and what
/// its output links keep of its output frames.
#[derive(Debug, Clone, Default)]
pub(crate) struct WhatTheNodeOfAHelperPlacedProcessorReaches {
    pub(crate) stream_facts_of_its_output_ports: Arc<StreamFactsOfOutputPorts>,
    pub(crate) upstream_stream_facts_by_its_inbound_links: Arc<UpstreamStreamFactsByInboundLink>,
    pub(crate) shared_native_service_leases: Arc<SharedNativeServiceLeasesOfOneHelper>,
    pub(crate) failure_the_node_reported_of_itself: Arc<FailureTheNodeReportedOfItself>,
    pub(crate) process_raising_the_node_reports: Arc<ProcessRaisingTheNodeReports>,
    pub(crate) output_frames_kept_by_its_output_links:
        Arc<OutputFramesKeptByTheOutputLinksOfOneProcessor>,
}

/// Replace the stream facts the helper's output port carries.
pub(super) fn handle_set_output_port_stream_facts(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
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
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
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
    use crate::core::graph::{PortInfo, PortKind};
    use crate::iceoryx2::InboundLinkName;

    fn output_port_named(name: &str) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            description: String::new(),
            port_kind: PortKind::Data,
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            pin_default: None,
            family: crate::core::descriptors::PortFamilyDeclaration::of_family(PortFamily::Packets),
        }
    }

    fn dispatched(
        what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
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
        let what_the_writer_reaches = WhatTheNodeOfAHelperPlacedProcessorReaches {
            stream_facts_of_its_output_ports: Arc::new(StreamFactsOfOutputPorts::as_declared_by(
                &[output_port_named("encoded_video")],
                None,
            )),
            ..Default::default()
        };
        let what_the_reader_reaches = WhatTheNodeOfAHelperPlacedProcessorReaches::default();
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
        let what_the_node_reaches = WhatTheNodeOfAHelperPlacedProcessorReaches {
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
