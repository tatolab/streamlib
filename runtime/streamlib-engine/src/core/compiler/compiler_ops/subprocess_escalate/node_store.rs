// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A helper-placed node reading and writing its own keys of its project's store.

use super::port_stream_facts::WhatTheNodeOfAHelperPlacedProcessorReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestReadNodeStoreKey, EscalateRequestRemoveNodeStoreKey,
    EscalateRequestWriteNodeStoreKey,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::StoreScopedToThisNode;
use crate::core::error::{Error, Result};

/// Answer the value the node keeps under the key, absent when it keeps none.
pub(super) fn handle_read_node_store_key(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestReadNodeStoreKey,
) -> EscalateResponse {
    match the_store_of_the_node(what_the_node_reaches)
        .and_then(|store_of_the_node| store_of_the_node.read(&request.key))
    {
        Ok(node_store_value) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            node_store_value,
            ..Default::default()
        }),
        Err(refusal) => the_refusal(rid, refusal),
    }
}

/// Keep the value under the key.
pub(super) fn handle_write_node_store_key(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestWriteNodeStoreKey,
) -> EscalateResponse {
    match the_store_of_the_node(what_the_node_reaches)
        .and_then(|store_of_the_node| store_of_the_node.write(&request.key, &request.value))
    {
        Ok(()) => the_acknowledgement(rid),
        Err(refusal) => the_refusal(rid, refusal),
    }
}

/// Stop keeping the key.
pub(super) fn handle_remove_node_store_key(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
    rid: String,
    request: EscalateRequestRemoveNodeStoreKey,
) -> EscalateResponse {
    match the_store_of_the_node(what_the_node_reaches)
        .and_then(|store_of_the_node| store_of_the_node.remove(&request.key))
    {
        Ok(()) => the_acknowledgement(rid),
        Err(refusal) => the_refusal(rid, refusal),
    }
}

fn the_store_of_the_node(
    what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
) -> Result<&StoreScopedToThisNode> {
    what_the_node_reaches
        .store_of_the_node
        .as_ref()
        .ok_or_else(|| {
            Error::Runtime(
                "this node has no store: its stream keeps no project store for it".to_string(),
            )
        })
}

fn the_acknowledgement(rid: String) -> EscalateResponse {
    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        ..Default::default()
    })
}

fn the_refusal(rid: String, refusal: Error) -> EscalateResponse {
    EscalateResponse::Err(EscalateResponseErr {
        request_id: rid,
        message: refusal.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::context::ProjectStoresOpenInThisRuntime;

    fn the_value_read_back(
        what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
        key: &str,
    ) -> Option<serde_json::Value> {
        match handle_read_node_store_key(
            what_the_node_reaches,
            "r-read".to_string(),
            EscalateRequestReadNodeStoreKey {
                key: key.to_string(),
                request_id: "r-read".to_string(),
            },
        ) {
            EscalateResponse::Ok(ok) => ok.node_store_value,
            EscalateResponse::Err(refused) => panic!("the read is refused: {}", refused.message),
        }
    }

    #[test]
    fn a_helpers_node_reads_back_what_it_wrote_until_it_removes_it() {
        let project_directory = tempfile::tempdir().expect("a temporary project");
        let project_stores = Arc::new(ProjectStoresOpenInThisRuntime::default());
        let what_the_node_reaches = WhatTheNodeOfAHelperPlacedProcessorReaches {
            store_of_the_node: Some(StoreScopedToThisNode::of_the_node(
                project_stores
                    .the_store_of_the_project_at(project_directory.path())
                    .expect("the store opens"),
                uuid::Uuid::new_v4(),
            )),
            ..Default::default()
        };
        assert_eq!(
            the_value_read_back(&what_the_node_reaches, "calibration"),
            None
        );

        let written = serde_json::json!({"gain": 0.75, "labels": ["left", "right"]});
        assert!(matches!(
            handle_write_node_store_key(
                &what_the_node_reaches,
                "r-write".to_string(),
                EscalateRequestWriteNodeStoreKey {
                    key: "calibration".to_string(),
                    request_id: "r-write".to_string(),
                    value: written.clone(),
                },
            ),
            EscalateResponse::Ok(ok) if ok.request_id == "r-write"
        ));
        assert_eq!(
            the_value_read_back(&what_the_node_reaches, "calibration"),
            Some(written)
        );

        assert!(matches!(
            handle_remove_node_store_key(
                &what_the_node_reaches,
                "r-remove".to_string(),
                EscalateRequestRemoveNodeStoreKey {
                    key: "calibration".to_string(),
                    request_id: "r-remove".to_string(),
                },
            ),
            EscalateResponse::Ok(_)
        ));
        assert_eq!(
            the_value_read_back(&what_the_node_reaches, "calibration"),
            None
        );
    }

    #[test]
    fn a_node_given_no_store_is_refused_by_name() {
        let answer = handle_read_node_store_key(
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            "r-read".to_string(),
            EscalateRequestReadNodeStoreKey {
                key: "calibration".to_string(),
                request_id: "r-read".to_string(),
            },
        );
        assert!(
            matches!(&answer, EscalateResponse::Err(refused) if refused.request_id == "r-read" && refused.message.contains("no store")),
            "{answer:?}"
        );
    }
}
