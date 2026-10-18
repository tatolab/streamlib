// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A helper's catalog-entry ops: replacing an output port's entry, and reading
//! the entry of the output port feeding one of its inputs. Neither reads a
//! payload.

use std::sync::Arc;

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestReadInboundLinkCatalogEntry, EscalateRequestSetOutputPortCatalogEntry,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::descriptors::CatalogEntry;
use crate::core::output_port_catalog_entries::{
    CatalogEntriesOfOutputPorts, UpstreamCatalogEntriesByInboundLink,
};

/// The catalog entries of the node a helper-placed processor runs for: its own
/// output ports' and the upstream ones its inbound links reach.
#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogEntriesAHelperPlacedProcessorReaches {
    pub(crate) catalog_entries_of_its_output_ports: Arc<CatalogEntriesOfOutputPorts>,
    pub(crate) upstream_catalog_entries_by_its_inbound_links:
        Arc<UpstreamCatalogEntriesByInboundLink>,
}

/// Replace the entry the helper's output port carries.
pub(super) fn handle_set_output_port_catalog_entry(
    catalog_entries: &CatalogEntriesAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestSetOutputPortCatalogEntry,
) -> EscalateResponse {
    let EscalateRequestSetOutputPortCatalogEntry {
        request_id: _,
        port,
        section,
        entry,
    } = request;
    match catalog_entries
        .catalog_entries_of_its_output_ports
        .replace_entry_of(&port, CatalogEntry { section, entry })
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

/// Answer with the current entry of the output port feeding the helper's input
/// port, or with none.
pub(super) fn handle_read_inbound_link_catalog_entry(
    catalog_entries: &CatalogEntriesAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestReadInboundLinkCatalogEntry,
) -> EscalateResponse {
    let EscalateRequestReadInboundLinkCatalogEntry {
        request_id: _,
        port,
        inbound_link,
    } = request;
    match catalog_entries
        .upstream_catalog_entries_by_its_inbound_links
        .catalog_entry_of_inbound_link(&port, inbound_link.as_deref())
    {
        Ok(catalog_entry) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            catalog_entry,
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
    use crate::core::descriptors::CatalogSection;
    use crate::core::graph::{PortInfo, PortKind};
    use crate::iceoryx2::InboundLinkName;

    fn output_port_named(name: &str) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            description: String::new(),
            port_kind: PortKind::Data,
            delivery_profile: None,
            audio_window: None,
            catalog_entry: None,
        }
    }

    fn dispatched(
        catalog_entries: &CatalogEntriesAHelperPlacedProcessorReaches,
        request: serde_json::Value,
    ) -> EscalateResponse {
        match serde_json::from_value::<EscalateRequest>(request)
            .expect("the request decodes from its wire form")
        {
            EscalateRequest::SetOutputPortCatalogEntry(request) => {
                handle_set_output_port_catalog_entry(
                    catalog_entries,
                    request.request_id.clone(),
                    request,
                )
            }
            EscalateRequest::ReadInboundLinkCatalogEntry(request) => {
                handle_read_inbound_link_catalog_entry(
                    catalog_entries,
                    request.request_id.clone(),
                    request,
                )
            }
            other => panic!("not a catalog-entry op: {other:?}"),
        }
    }

    #[test]
    fn a_helpers_set_replaces_its_ports_entry_and_a_downstream_read_sees_it() {
        let catalog_entries_of_the_writer = CatalogEntriesAHelperPlacedProcessorReaches {
            catalog_entries_of_its_output_ports: Arc::new(
                CatalogEntriesOfOutputPorts::as_declared_by(&[output_port_named("encoded_video")]),
            ),
            ..Default::default()
        };
        let catalog_entries_of_the_reader = CatalogEntriesAHelperPlacedProcessorReaches::default();
        catalog_entries_of_the_reader
            .upstream_catalog_entries_by_its_inbound_links
            .record_inbound_link(
                "video_in",
                &crate::core::graph::LinkUniqueId::from("link-1"),
                InboundLinkName::from("encoder/encoded_video"),
                "encoded_video",
                Arc::clone(&catalog_entries_of_the_writer.catalog_entries_of_its_output_ports),
            );

        let set = dispatched(
            &catalog_entries_of_the_writer,
            json!({
                "op": "set_output_port_catalog_entry",
                "request_id": "set-1",
                "port": "encoded_video",
                "section": "video",
                "entry": {"codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720},
            }),
        );
        assert!(matches!(set, EscalateResponse::Ok(_)), "got {set:?}");

        let read = dispatched(
            &catalog_entries_of_the_reader,
            json!({
                "op": "read_inbound_link_catalog_entry",
                "request_id": "read-1",
                "port": "video_in",
            }),
        );
        let EscalateResponse::Ok(answer) = read else {
            panic!("the read is answered ok; got {read:?}");
        };
        assert_eq!(
            answer.catalog_entry,
            Some(CatalogEntry {
                section: CatalogSection::Video,
                entry: json!({"codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720}),
            })
        );
    }

    #[test]
    fn a_set_hang_does_not_read_is_refused_naming_the_port_and_the_field() {
        let catalog_entries = CatalogEntriesAHelperPlacedProcessorReaches {
            catalog_entries_of_its_output_ports: Arc::new(
                CatalogEntriesOfOutputPorts::as_declared_by(&[output_port_named("frames")]),
            ),
            ..Default::default()
        };
        let refused = dispatched(
            &catalog_entries,
            json!({
                "op": "set_output_port_catalog_entry",
                "request_id": "set-2",
                "port": "frames",
                "section": "binary",
                "entry": {"mime": "application/msgpack"},
            }),
        );
        let EscalateResponse::Err(refusal) = refused else {
            panic!("an entry hang does not read is refused; got {refused:?}");
        };
        assert_eq!(refusal.request_id, "set-2");
        assert!(
            refusal.message.contains("frames") && refusal.message.contains("mode"),
            "got {}",
            refusal.message
        );
        assert_eq!(
            catalog_entries
                .catalog_entries_of_its_output_ports
                .current_entry_of("frames"),
            None
        );
    }
}
