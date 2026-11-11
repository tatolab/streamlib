// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::json;

use super::{ProjectStoresOpenInThisRuntime, StoreScopedToThisNode};

#[test]
fn two_runtimes_holding_one_project_share_its_store() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let first_runtime = ProjectStoresOpenInThisRuntime::default();
    let second_runtime = ProjectStoresOpenInThisRuntime::default();
    let node_uuid = uuid::Uuid::new_v4();
    let from_the_first = StoreScopedToThisNode::of_the_node(
        first_runtime
            .the_store_of_the_project_at(project_directory.path())
            .expect("the first runtime opens the store"),
        node_uuid,
    );
    let from_the_second = StoreScopedToThisNode::of_the_node(
        second_runtime
            .the_store_of_the_project_at(project_directory.path())
            .expect("the second runtime shares the store"),
        node_uuid,
    );
    from_the_first.write("value", &json!(0.4)).expect("writes");
    assert_eq!(
        from_the_second.read("value").expect("reads"),
        Some(json!(0.4))
    );
}
