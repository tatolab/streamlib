// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `set_node_type_group` over a real engine: a node type of any source filed under a group of
//! the project's own, served as its `category` by `list_node_types` and `describe_node_types`,
//! put back where its author files it, each one undo; an import's
//! map filing the import's own node types unless the project files them itself; a filing taken
//! away once its node type is gone; the feed announcing a filing; and the refusals.

use super::input_value_set_tests::{InputValueTestNode, register_the_input_value_test_node_once};
use super::node_type_reads_tests::{a_project_listing, the_project_source};
use super::project_node_type_listing_tests::{
    STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING, a_listed_node_type,
    the_next_node_types_announced, the_node_types_named_by,
    write_the_listing_of_one_distribution_offering,
};
use super::*;

use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

/// A distribution installed in the project's venv, declaring one node type.
fn an_installed_distribution_source() -> Value {
    json!({
        "source": "installed_distribution", "distribution": "tatolab-lights",
        "distribution_version": "1.2.0",
        "node_types": [{
            "type": "tatolab_lights:Dimmer", "kind": "process", "description": "A dimmer.",
            "category": "Lights", "inputs": null, "outputs": null, "path": null, "line": null,
        }],
        "graph_generators": [], "unreadable_modules": [],
    })
}

/// The `category` and `category_from` the node type `node_type` is listed with in `listed`.
fn listed_filing(listed: &Value, node_type: &str) -> (Value, Value) {
    let listed_type = listed["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|source| source["node_types"].as_array().into_iter().flatten())
        .find(|listed_type| {
            listed_type["node_type"] == node_type || listed_type["type"] == node_type
        })
        .unwrap_or_else(|| panic!("`{node_type}` is not listed: {listed}"));
    (
        listed_type["category"].clone(),
        listed_type["category_from"].clone(),
    )
}

/// A project node type, an installed distribution's and a built-in are each filed under a group
/// of the project's own, which every reading serves; clearing one puts it back under its
/// author's group, and each undo takes one change back.
///
/// Mental-revert: listing the declared `category` alone leaves `Mixer` under `Audio`.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_type_of_each_source_is_filed_under_the_projects_group_and_undoes() {
    register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([
        the_project_source(),
        an_installed_distribution_source()
    ]));
    let built_in = InputValueTestNode::node_class_import_path()
        .as_str()
        .to_owned();
    let file_under = |node_type: &str, group: Value| {
        let engine = &engine;
        let arguments = json!({
            "project_directory": project.path(), "node_type": node_type, "group": group,
            "author": an_agent_named("alice"),
        });
        async move {
            engine
                .the_answer_or_the_refusal_of("set_node_type_group", arguments)
                .await
        }
    };
    let list = |operation: &'static str| {
        let engine = &engine;
        let arguments = json!({ "project_directory": project.path() });
        async move {
            engine
                .the_answer_or_the_refusal_of(operation, arguments)
                .await
                .expect("the node types are listed")
        }
    };

    let filed = file_under("nodes.mix:Mixer", json!("Show/Levels"))
        .await
        .expect("the project's own node type is filed");
    assert_eq!(filed["node_type"], "nodes.mix:Mixer", "{filed}");
    assert_eq!(filed["group"], "Show/Levels", "{filed}");
    assert!(filed["transaction"].is_string(), "{filed}");
    file_under("tatolab_lights:Dimmer", json!("Show"))
        .await
        .expect("the installed distribution's node type is filed");
    file_under(&built_in, json!("Show/Inputs"))
        .await
        .expect("the built-in is filed");

    let listed = list("list_node_types").await;
    assert_eq!(
        listed_filing(&listed, "nodes.mix:Mixer"),
        (json!("Show/Levels"), json!("project"))
    );
    assert_eq!(
        listed_filing(&listed, "tatolab_lights:Dimmer"),
        (json!("Show"), json!("project"))
    );
    assert_eq!(
        listed_filing(&listed, &built_in),
        (json!("Show/Inputs"), json!("project"))
    );
    let described = engine
        .the_answer_or_the_refusal_of(
            "describe_node_types",
            json!({ "project_directory": project.path(), "node_types": [built_in] }),
        )
        .await
        .expect("the built-in is described");
    assert_eq!(
        (
            described["node_types"][0]["category"].clone(),
            described["node_types"][0]["category_from"].clone()
        ),
        (json!("Show/Inputs"), json!("project")),
        "{described}"
    );
    let project_file: Value = serde_json::from_slice(
        &std::fs::read(project.path().join("tatolab-project.json")).expect("the project file"),
    )
    .expect("the project file is JSON");
    assert_eq!(
        project_file["node_type_groups"]["nodes.mix:Mixer"], "Show/Levels",
        "the move is saved in the project: {project_file}"
    );

    let cleared = file_under("nodes.mix:Mixer", Value::Null)
        .await
        .expect("the filing is taken away");
    assert_eq!(cleared["group"], Value::Null, "{cleared}");
    assert_eq!(
        listed_filing(&list("list_node_types").await, "nodes.mix:Mixer"),
        (json!("Audio"), json!("declaration")),
        "back under its author's group"
    );

    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the clearing undoes");
    assert_eq!(
        listed_filing(&list("list_node_types").await, "nodes.mix:Mixer"),
        (json!("Show/Levels"), json!("project"))
    );
    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the built-in's move undoes");
    assert_eq!(
        listed_filing(&list("list_node_types").await, &built_in),
        (Value::Null, Value::Null),
        "the built-in declares no group"
    );
}

/// An import's own map files the import's own node types, by the name it gives them, until the
/// importing project files one itself, and files nothing else the project offers.
///
/// Mental-revert: reading only the project's own map lists the gripper under `Grip`; reading an
/// import's map for every node type lists the built-in under `Arm`.
#[tokio::test(flavor = "multi_thread")]
async fn an_imports_map_files_the_node_types_it_names_and_the_projects_own_map_wins() {
    register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let built_in = InputValueTestNode::node_class_import_path()
        .as_str()
        .to_owned();
    let arm = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let mut arm_groups = serde_json::Map::new();
    arm_groups.insert("nodes.gripper:Gripper".to_owned(), json!("Robot/Hands"));
    arm_groups.insert(built_in.clone(), json!("Arm"));
    std::fs::write(
        arm.path().join("tatolab-project.json"),
        json!({
            "project": arm.project, "name": "arm", "dependencies": [],
            "node_type_groups": arm_groups,
        })
        .to_string(),
    )
    .expect("the arm's project file");
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    write_an_executable_script_from_a_child_process(
        &cell.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    let added = engine
        .the_answer_or_the_refusal_of(
            "add_dependency",
            json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the arm is imported");
    let folder = added["dependency"]["folder"]
        .as_str()
        .expect("the import's folder")
        .to_owned();
    let gripper = format!("imports/{folder}/nodes.gripper:Gripper");
    std::fs::write(
        cell.path().join("listing.json"),
        json!({ "sources": [{
            "source": "dependency", "project": arm.project, "folder": folder,
            "version": added["dependency"]["version"],
            "node_types": [{
                "type": gripper, "kind": "process", "description": "A gripper.",
                "category": "Grip", "inputs": [], "outputs": [], "path": "nodes/gripper.py",
                "line": 1,
            }],
            "graph_generators": [], "unreadable_modules": [],
        }] })
        .to_string(),
    )
    .expect("the listing document");
    let list = || {
        let engine = &engine;
        let arguments = json!({ "project_directory": cell.path() });
        async move {
            engine
                .the_answer_or_the_refusal_of("list_node_types", arguments)
                .await
                .expect("the node types are listed")
        }
    };

    let listed = list().await;
    assert_eq!(
        listed_filing(&listed, &gripper),
        (json!("Robot/Hands"), json!("import"))
    );
    assert_eq!(
        listed_filing(&listed, &built_in),
        (Value::Null, Value::Null),
        "a built-in is not the import's to file"
    );

    engine
        .the_answer_or_the_refusal_of(
            "set_node_type_group",
            json!({ "project_directory": cell.path(), "node_type": built_in, "group": "Cell",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the cell files the built-in itself");
    engine
        .the_answer_or_the_refusal_of(
            "set_node_type_group",
            json!({ "project_directory": cell.path(), "node_type": gripper, "group": "Cell",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the cell files the import's gripper itself");
    let listed = list().await;
    assert_eq!(
        listed_filing(&listed, &built_in),
        (json!("Cell"), json!("project"))
    );
    assert_eq!(
        listed_filing(&listed, &gripper),
        (json!("Cell"), json!("project"))
    );
}

/// A filing outlives the node type it names: once the project offers it no more, a `null` group
/// still takes the filing away, and a filing sent again under its key answers what it applied.
///
/// Mental-revert: refusing a node type not offered whatever the group leaves `nodes.mix:Mixer`
/// in the project file for good; refusing before the key is read answers the retry
/// `node_type_not_found`.
#[tokio::test(flavor = "multi_thread")]
async fn a_filing_of_a_node_type_the_project_offers_no_more_is_still_taken_away() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([the_project_source()]));
    let file_under = |group: Value, idempotency_key: Value| {
        let engine = &engine;
        let arguments = json!({
            "project_directory": project.path(), "node_type": "nodes.mix:Mixer", "group": group,
            "author": an_agent_named("alice"), "idempotency_key": idempotency_key,
        });
        async move {
            engine
                .the_answer_or_the_refusal_of("set_node_type_group", arguments)
                .await
        }
    };
    let filed = file_under(json!("Show"), json!("file-the-mixer"))
        .await
        .expect("the project's node type is filed");
    std::fs::write(
        project.path().join("listing.json"),
        json!({ "sources": [] }).to_string(),
    )
    .expect("the mixer's module is gone from the listing");

    let sent_again = file_under(json!("Show"), json!("file-the-mixer"))
        .await
        .expect("a filing sent again under its key answers what it applied");
    assert_eq!(
        sent_again["transaction"], filed["transaction"],
        "{sent_again}"
    );
    file_under(json!("Show"), Value::Null)
        .await
        .expect_err("a node type offered no more is not filed again");
    let cleared = file_under(Value::Null, Value::Null)
        .await
        .expect("the filing is taken away");

    assert_eq!(cleared["group"], Value::Null, "{cleared}");
    let project_file =
        std::fs::read_to_string(project.path().join("tatolab-project.json")).expect("the file");
    assert!(
        !project_file.contains("node_type_groups"),
        "nothing is left filed: {project_file}"
    );
}

/// A node type filed under a group of the project's own is announced on the project's feed as
/// updated, carrying the group it is now filed under, as the project file the filing writes is
/// seen to change.
///
/// Mental-revert: leaving `tatolab-project.json` out of the files that can change the node types
/// announces nothing after the filing.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_type_filed_under_a_group_is_announced_updated_on_the_projects_feed() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    write_an_executable_script_from_a_child_process(
        &cell.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    let handler = crate::mcp::LocalApiMcpServerHandler::new(
        engine.operations_on_the_loaded_streams(),
        tokio_util::sync::CancellationToken::new(),
    );
    let dispatcher = handler.control_operation_dispatcher();
    let _files_watched = dispatcher
        .project_repositories
        .watch_the_files_of(cell.project);
    let mut change_feed = dispatcher
        .project_repositories
        .subscribe_to_the_change_feed();
    write_the_listing_of_one_distribution_offering(
        &cell,
        json!([a_listed_node_type(
            "tatolab.camera:CameraSource",
            "a camera"
        )]),
    );
    dispatcher
        .the_node_types_the_project_offers_read_from_source(
            "list_node_types",
            cell.path().to_path_buf(),
        )
        .await
        .expect("the cell's node types are listed");

    dispatcher
        .set_node_type_group(
            streamlib_control_contract::SetNodeTypeGroupRequest {
                project_directory: cell.path().to_path_buf(),
                node_type: "tatolab.camera:CameraSource".to_owned(),
                group: Some("Show/Inputs".to_owned()),
                author: None,
                idempotency_key: None,
            },
            &crate::control_operation_dispatcher::ControlRequestOrigin {
                client_name: Some("alice".to_owned()),
                subscriptions: None,
                viewers_held_open: None,
            },
        )
        .await
        .expect("the camera is filed");

    let updated = the_next_node_types_announced(&mut change_feed).await;
    assert_eq!(
        the_node_types_named_by(&updated),
        ("updated", vec!["tatolab.camera:CameraSource".to_owned()])
    );
    let streamlib_project_repository::ProjectChangeOnTheFeed::NodeTypesUpdated(updated) = updated
    else {
        unreachable!()
    };
    assert_eq!(
        (
            updated[0].node_type.category.as_deref(),
            updated[0].node_type.category_from
        ),
        (
            Some("Show/Inputs"),
            Some(streamlib_runtime_client_contract::local_api_wire_contract::NodeTypeCategoryFrom::Project)
        )
    );
}

/// A node type neither the project nor the runtime offers is refused suggesting the listing, and
/// a group that is no path in the runtime's words, each with nothing recorded.
///
/// Mental-revert: filing any name saves `nodes.gone:Gone` in the project file.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_type_not_offered_and_a_group_that_is_no_path_are_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([the_project_source()]));

    let not_offered = engine
        .the_answer_or_the_refusal_of(
            "set_node_type_group",
            json!({ "project_directory": project.path(), "node_type": "nodes.gone:Gone",
                    "group": "Show" }),
        )
        .await
        .expect_err("nothing offers `nodes.gone:Gone`");
    assert_eq!(
        not_offered.refusal_code,
        ControlOperationRefusalCode::NodeTypeNotFound,
        "{not_offered:?}"
    );
    assert_eq!(
        not_offered
            .suggested_call
            .as_ref()
            .map(|call| call.operation.as_str()),
        Some("list_node_types"),
        "{not_offered:?}"
    );

    let no_path = engine
        .the_answer_or_the_refusal_of(
            "set_node_type_group",
            json!({ "project_directory": project.path(), "node_type": "nodes.mix:Mixer",
                    "group": "Show//Levels" }),
        )
        .await
        .expect_err("a blank name in the path");
    assert!(
        no_path.runtime_words.contains("none of them blank"),
        "{no_path:?}"
    );
    let project_file =
        std::fs::read_to_string(project.path().join("tatolab-project.json")).expect("the file");
    assert!(
        !project_file.contains("node_type_groups"),
        "nothing is recorded: {project_file}"
    );
}
