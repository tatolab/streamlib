// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::test_support::{
    THE_REFUSAL_OF_A_PORT_DECLARED_BY_FAMILY, a_view_that_does_not_open_in,
};

/// A project whose view does not open, beside a node type its interpreter describes, opens with
/// that type described and only the view's node held, in the `Error` state, carrying the view's
/// refusal; the stream's failure names the view and its refusal once. The stream places the view
/// by `<project name>:<view folder>` and reads it as the view of the project's UUID.
///
/// Mental-revert: refusing every type of the describe for the one view that does not open holds
/// the described type's node as a placeholder carrying the view's refusal too.
#[test]
#[serial]
fn a_view_that_does_not_open_holds_only_its_own_node_and_the_rest_is_described() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let described_type = NodeClassImportPath::new("nodes.cell:Cell").unwrap();
    let view_type_placed_by_its_projects_name = format!("{TEST_PROJECT_NAME}:stage");
    let mut graph = the_graph_of_a_stream_named("camera");
    graph["nodes"].as_array_mut().unwrap().extend([
        serde_json::json!({"name": "cell", "type": described_type.as_str()}),
        serde_json::json!({"name": "stage", "type": view_type_placed_by_its_projects_name}),
    ]);
    let project = ProjectWithGraphDocuments::holding(graph);
    a_view_that_does_not_open_in(project.path(), "stage");
    let view_type_read = streamlib_project_repository::the_view_type_of(project.project, "stage");
    let describe_document = serde_json::json!({
        "described_node_types": [{
            "import_path": described_type.as_str(),
            "short_name": "Cell",
            "description": "a cell",
            "kind": "process",
            "execution": {"mode": "reactive"},
            "scheduling_priority": null,
            "paced_by_real_time": false,
            "config_schema": {"type": "object"},
            "input_ports": [{"name": "video", "description": "",
                             "family": {"family": "records"}, "delivery_profile": "newest"}],
            "output_ports": [],
        }],
        "refused_node_types": [],
    });
    let no_installed_modules = serde_json::json!({
        "described_node_types": [], "refused_node_types": [], "installed_modules": [],
    });
    write_an_executable_script_from_a_child_process(
        &project.interpreter(),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n*--describe-installed-modules*)\n\
             cat <<'DESCRIBED'\n{no_installed_modules}\nDESCRIBED\n;;\n\
             *--describe*)\ncat <<'DESCRIBED'\n{describe_document}\nDESCRIBED\n;;\n\
             esac\nexit 0\n"
        ),
    );
    let camera = project.stream_id_of("camera");

    a_project_opened_on(&runner, &project);

    let held = runner
        .loaded_stream_of_the_id(&camera)
        .expect("the stream is held from its document");
    let node_types = held.node_types_this_stream_resolves();
    assert!(
        node_types.descriptor(&described_type).is_some(),
        "the type beside the view is described"
    );
    assert!(
        node_types
            .placeholder_of_the_node_type_not_described(&described_type)
            .is_none()
    );
    let stage_placeholder = node_types
        .placeholder_of_the_node_type_not_described(
            &NodeClassImportPath::new(&view_type_read).unwrap(),
        )
        .expect("the view's node is held as a placeholder");
    assert!(
        stage_placeholder
            .why_its_node_code_did_not_describe_it
            .contains(THE_REFUSAL_OF_A_PORT_DECLARED_BY_FAMILY),
        "{}",
        stage_placeholder.why_its_node_code_did_not_describe_it
    );
    let render = held.to_json().unwrap();
    let state_of = |node_name: &str| {
        render["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["name"] == node_name)
            .unwrap_or_else(|| panic!("`graph` shows no node `{node_name}`: {render}"))
            ["components"]["state"]
            .clone()
    };
    assert_eq!(state_of("stage"), "Error", "{render}");
    assert_ne!(state_of("cell"), "Error", "{render}");
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    let failed_because = listed.failed_because.as_deref().unwrap_or_default();
    assert!(
        failed_because.contains(&format!("`{view_type_read}`")),
        "{failed_because}"
    );
    assert_eq!(
        failed_because
            .matches(THE_REFUSAL_OF_A_PORT_DECLARED_BY_FAMILY)
            .count(),
        1,
        "{failed_because}"
    );
    assert!(
        !failed_because.contains(described_type.as_str()),
        "{failed_because}"
    );
}
