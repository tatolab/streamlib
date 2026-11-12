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

#[test]
fn a_project_store_closes_when_the_last_runtime_holding_it_lets_go() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let first_runtime = ProjectStoresOpenInThisRuntime::default();
    let second_runtime = ProjectStoresOpenInThisRuntime::default();
    first_runtime
        .the_store_of_the_project_at(project_directory.path())
        .expect("the first runtime opens the store");
    second_runtime
        .the_store_of_the_project_at(project_directory.path())
        .expect("the second runtime shares the store");

    drop(first_runtime);
    assert!(
        streamlib_project_repository::ProjectStore::open_in_the_project_at(
            project_directory.path()
        )
        .is_err(),
        "the store stays open while the second runtime holds it"
    );
    drop(second_runtime);
    streamlib_project_repository::ProjectStore::open_in_the_project_at(project_directory.path())
        .expect("the store's file opens again once every runtime has let it go");
}

#[test]
fn closing_a_project_releases_its_store_once_no_other_runtime_holds_it() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let runtime = ProjectStoresOpenInThisRuntime::default();
    let from_the_runtime = StoreScopedToThisNode::of_the_node(
        runtime
            .the_store_of_the_project_at(project_directory.path())
            .expect("the runtime opens the store"),
        uuid::Uuid::new_v4(),
    );
    from_the_runtime
        .write("value", &json!(0.7))
        .expect("writes");
    drop(from_the_runtime);

    runtime.close_the_store_of_the_project_at(project_directory.path());
    let reopened = streamlib_project_repository::ProjectStore::open_in_the_project_at(
        project_directory.path(),
    )
    .expect("the store's file opens once its project closed");
    drop(reopened);
    assert!(
        runtime
            .the_store_of_the_project_at(project_directory.path())
            .is_ok(),
        "a project opened again opens its store again"
    );
}
