// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

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
) -> Result<StoreScopedToThisNode> {
    what_the_node_reaches
        .store_of_the_node
        .as_ref()
        .ok_or_else(|| {
            Error::Runtime(
                "this helper was spawned by no node's context, so it has no store".to_string(),
            )
        })?
        .open()
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
    use crate::core::context::{ProjectStoresOpenInThisRuntime, StoreOfOneNodeOpenedOnFirstUse};

    fn what_a_node_of_the_project_at_reaches(
        project_stores: &Arc<ProjectStoresOpenInThisRuntime>,
        project_directory: &std::path::Path,
        node_uuid: uuid::Uuid,
    ) -> WhatTheNodeOfAHelperPlacedProcessorReaches {
        WhatTheNodeOfAHelperPlacedProcessorReaches {
            store_of_the_node: Some(StoreOfOneNodeOpenedOnFirstUse {
                project_stores: Arc::clone(project_stores),
                project_directory: Arc::from(project_directory),
                node_uuid: Some(node_uuid),
            }),
            ..Default::default()
        }
    }

    fn the_read_answer(
        what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
        key: &str,
    ) -> EscalateResponse {
        handle_read_node_store_key(
            what_the_node_reaches,
            "r-read".to_string(),
            EscalateRequestReadNodeStoreKey {
                key: key.to_string(),
                request_id: "r-read".to_string(),
            },
        )
    }

    fn the_value_read_back(
        what_the_node_reaches: &WhatTheNodeOfAHelperPlacedProcessorReaches,
        key: &str,
    ) -> Option<serde_json::Value> {
        match the_read_answer(what_the_node_reaches, key) {
            EscalateResponse::Ok(ok) => ok.node_store_value,
            EscalateResponse::Err(refused) => panic!("the read is refused: {}", refused.message),
        }
    }

    #[test]
    fn a_helpers_node_writes_its_own_keys_of_the_project_store_until_it_removes_them() {
        let project_directory = tempfile::tempdir().expect("a temporary project");
        let project_stores = Arc::new(ProjectStoresOpenInThisRuntime::default());
        let node_uuid = uuid::Uuid::new_v4();
        let what_the_node_reaches = what_a_node_of_the_project_at_reaches(
            &project_stores,
            project_directory.path(),
            node_uuid,
        );
        let what_another_node_reaches = what_a_node_of_the_project_at_reaches(
            &project_stores,
            project_directory.path(),
            uuid::Uuid::new_v4(),
        );
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
            Some(written.clone())
        );
        let the_nodes_own_keys = StoreScopedToThisNode::of_the_node(
            project_stores
                .the_store_of_the_project_at(project_directory.path())
                .expect("the store opens"),
            node_uuid,
        );
        assert_eq!(
            the_nodes_own_keys
                .read("calibration")
                .expect("the key reads"),
            Some(written)
        );
        assert_eq!(
            the_value_read_back(&what_another_node_reaches, "calibration"),
            None
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
    fn a_store_that_does_not_open_is_refused_in_its_own_words() {
        let not_a_directory =
            tempfile::NamedTempFile::new().expect("a file where a project would be");
        let project_stores = Arc::new(ProjectStoresOpenInThisRuntime::default());
        let what_the_node_reaches = what_a_node_of_the_project_at_reaches(
            &project_stores,
            not_a_directory.path(),
            uuid::Uuid::new_v4(),
        );
        let opening_it_directly = StoreOfOneNodeOpenedOnFirstUse {
            project_stores: Arc::clone(&project_stores),
            project_directory: Arc::from(not_a_directory.path()),
            node_uuid: Some(uuid::Uuid::new_v4()),
        }
        .open()
        .expect_err("a file is no project directory")
        .to_string();

        let answer = the_read_answer(&what_the_node_reaches, "calibration");
        assert!(
            matches!(&answer, EscalateResponse::Err(refused) if refused.message == opening_it_directly),
            "{answer:?}"
        );
    }

    #[test]
    fn a_helper_no_nodes_context_spawned_is_refused_by_name() {
        let answer = the_read_answer(
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            "calibration",
        );
        assert!(
            matches!(&answer, EscalateResponse::Err(refused) if refused.request_id == "r-read" && refused.message.contains("no store")),
            "{answer:?}"
        );
    }
}
