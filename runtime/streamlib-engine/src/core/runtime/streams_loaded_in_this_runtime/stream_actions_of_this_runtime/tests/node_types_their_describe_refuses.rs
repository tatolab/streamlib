// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// What the stub interpreter says it refused `nodes.broken:Broken` for.
const THE_REFUSAL_OF_THE_BROKEN_TYPE: &str = "nodes.broken:Broken's input `view` is optional, and a function node takes a value on every input";

/// A project whose interpreter refuses one of its node types opens with the one beside it, and
/// the one its imported folder holds, described, and only the refused type's node held, in the
/// `Error` state; the stream's failure names the refused type and its refusal, and no other node
/// or link.
///
/// Mental-revert: refusing every type of the describe for the one it refused, or stopping at the
/// first folder's refusal, holds the described types' nodes as placeholders too, and the failure
/// names whichever node comes first.
#[test]
#[serial]
fn a_type_its_describe_refuses_holds_only_its_own_node_and_the_rest_is_described() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let described_type = NodeClassImportPath::new("nodes.cell:Cell").unwrap();
    let refused_type = NodeClassImportPath::new("nodes.broken:Broken").unwrap();
    let imported_type =
        NodeClassImportPath::new("imports/core-nodes/core_nodes.clock:Clock").unwrap();
    let mut graph = the_graph_of_a_stream_named("camera");
    graph["nodes"].as_array_mut().unwrap().extend([
        serde_json::json!({"name": "cell", "type": described_type.as_str()}),
        serde_json::json!({"name": "broken", "type": refused_type.as_str()}),
        serde_json::json!({"name": "clock", "type": imported_type.as_str()}),
    ]);
    graph["links"] = serde_json::json!([
        {
            "source": { "node": "source", "port": "out1" },
            "target": { "node": "cell", "port": "video" },
        },
        {
            "source": { "node": "source", "port": "out2" },
            "target": { "node": "broken", "port": "view" },
        },
    ]);
    let project = ProjectWithGraphDocuments::holding(graph);
    let a_described_type = |node_type: &NodeClassImportPath, short_name: &str| {
        serde_json::json!({
            "import_path": node_type.as_str(),
            "short_name": short_name,
            "description": short_name,
            "kind": "process",
            "execution": {"mode": "reactive"},
            "scheduling_priority": null,
            "paced_by_real_time": false,
            "config_schema": {"type": "object"},
            "input_ports": [{"name": "video", "description": "",
                             "family": {"family": "records"}, "delivery_profile": "newest"}],
            "output_ports": [],
        })
    };
    let describe_document = serde_json::json!({
        "described_node_types": [
            a_described_type(&described_type, "Cell"),
            a_described_type(&imported_type, "Clock"),
        ],
        "refused_node_types": [{
            "import_path": refused_type.as_str(),
            "refusal": THE_REFUSAL_OF_THE_BROKEN_TYPE,
        }],
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
    for described in [&described_type, &imported_type] {
        assert!(
            node_types.descriptor(described).is_some(),
            "`{described}` beside the refused type is described"
        );
        assert!(
            node_types
                .placeholder_of_the_node_type_not_described(described)
                .is_none()
        );
    }
    let refused_placeholder = node_types
        .placeholder_of_the_node_type_not_described(&refused_type)
        .expect("the refused type's node is held as a placeholder");
    assert!(
        refused_placeholder
            .why_its_node_code_did_not_describe_it
            .contains(THE_REFUSAL_OF_THE_BROKEN_TYPE),
        "{}",
        refused_placeholder.why_its_node_code_did_not_describe_it
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
    assert_eq!(state_of("broken"), "Error", "{render}");
    assert_ne!(state_of("cell"), "Error", "{render}");
    assert_ne!(state_of("clock"), "Error", "{render}");
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    let failed_because = listed.failed_because.as_deref().unwrap_or_default();
    assert!(
        failed_because.contains(&format!("`{}`", refused_type.as_str())),
        "{failed_because}"
    );
    assert!(failed_because.contains("`broken`"), "{failed_because}");
    assert!(
        failed_because.contains(THE_REFUSAL_OF_THE_BROKEN_TYPE),
        "{failed_because}"
    );
    assert!(
        !failed_because.contains(described_type.as_str())
            && !failed_because.contains(imported_type.as_str())
            && !failed_because.contains("link"),
        "{failed_because}"
    );
}
