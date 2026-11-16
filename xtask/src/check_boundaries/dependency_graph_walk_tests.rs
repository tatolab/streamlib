// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// ----- transitive trunk-set -> engine walk (synthetic metadata) -----

/// Build a synthetic `cargo metadata` document from workspace-member ids,
/// `(id, name)` package pairs, and `(from_id, to_id, kind)` resolve edges
/// where `kind` is `"normal"` (emitted as JSON `null`), `"build"`, or
/// `"dev"`. Enough of the real shape for [`NormalBuildDepGraph::from_metadata`].
fn synthetic_metadata(
    members: &[&str],
    names: &[(&str, &str)],
    edges: &[(&str, &str, &str)],
) -> serde_json::Value {
    use serde_json::json;
    let packages: Vec<_> = names
        .iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    let nodes: Vec<_> = names
        .iter()
        .map(|(id, _)| {
            let deps: Vec<_> = edges
                .iter()
                .filter(|(from, _, _)| from == id)
                .map(|(_, to, kind)| {
                    let kind_val = if *kind == "normal" {
                        serde_json::Value::Null
                    } else {
                        json!(kind)
                    };
                    json!({ "pkg": to, "dep_kinds": [ { "kind": kind_val } ] })
                })
                .collect();
            json!({ "id": id, "deps": deps })
        })
        .collect();
    json!({
        "workspace_members": members,
        "packages": packages,
        "resolve": { "nodes": nodes },
    })
}

#[test]
fn transitive_trunk_reaches_engine_through_intermediate_fails() {
    // macros -> intermediate -> engine, all normal edges: the walk must
    // find the chain AND print it end to end.
    let md = synthetic_metadata(
        &["sdk", "mid", "eng"],
        &[
            ("sdk", "streamlib-macros"),
            ("mid", "streamlib-intermediate"),
            ("eng", "streamlib-engine"),
        ],
        &[("sdk", "mid", "normal"), ("mid", "eng", "normal")],
    );
    let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
    let chains = find_trunk_engine_chains(&graph);
    assert_eq!(chains.len(), 1, "expected one chain, got {:?}", chains);
    assert_eq!(chains[0].root_crate, "streamlib-macros");
    assert_eq!(
        chains[0].display_chain(),
        "streamlib-macros -> streamlib-intermediate -> streamlib-engine",
    );
}

#[test]
fn transitive_trunk_reaches_engine_only_via_dev_edge_passes() {
    // macros -> engine exists ONLY through a dev-only edge; the shared
    // `dep_kinds` filter drops it, so no chain is reported. Locks the
    // dev-dep exemption — without the filter this would false-red.
    let md = synthetic_metadata(
        &["sdk", "eng"],
        &[("sdk", "streamlib-macros"), ("eng", "streamlib-engine")],
        &[("sdk", "eng", "dev")],
    );
    let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
    let chains = find_trunk_engine_chains(&graph);
    assert!(
        chains.is_empty(),
        "dev-only edge must not form a chain: {:?}",
        chains
    );
}

#[test]
fn transitive_consumer_rhi_reaches_engine_through_intermediate_fails() {
    // consumer-rhi is a trunk root (#1252 follow-up): its transitive
    // normal + build closure must also be walked. consumer-rhi -> mid ->
    // engine, all normal edges — the walk must find the chain and attribute
    // it to consumer-rhi. Locks consumer-rhi's root membership in the
    // transitive check specifically, distinct from the macros case.
    let md = synthetic_metadata(
        &["crhi", "mid", "eng"],
        &[
            ("crhi", "streamlib-consumer-rhi"),
            ("mid", "streamlib-intermediate"),
            ("eng", "streamlib-engine"),
        ],
        &[("crhi", "mid", "normal"), ("mid", "eng", "normal")],
    );
    let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
    let chains = find_trunk_engine_chains(&graph);
    assert_eq!(chains.len(), 1, "expected one chain, got {:?}", chains);
    assert_eq!(chains[0].root_crate, "streamlib-consumer-rhi");
    assert_eq!(
        chains[0].display_chain(),
        "streamlib-consumer-rhi -> streamlib-intermediate -> streamlib-engine",
    );
}

#[test]
fn transitive_trunk_clean_graph_passes() {
    // macros -> some other member, engine present but unreachable.
    let md = synthetic_metadata(
        &["sdk", "mid", "eng"],
        &[
            ("sdk", "streamlib-macros"),
            ("mid", "streamlib-other"),
            ("eng", "streamlib-engine"),
        ],
        &[("sdk", "mid", "normal")],
    );
    let graph = NormalBuildDepGraph::from_metadata(&md).unwrap();
    let chains = find_trunk_engine_chains(&graph);
    assert!(
        chains.is_empty(),
        "clean graph must yield no chain: {:?}",
        chains
    );
}

// ----- the engine-free clients link no engine (synthetic metadata) -----

/// Every workspace crate `find_engine_free_client_engine_chains` names, plus the transport crate `tatolab`
/// really links, as `(id, name)` pairs.
const ENGINE_FREE_CLIENT_FIXTURE_PACKAGES: &[(&str, &str)] = &[
    ("cli", "tatolab-cli"),
    ("contract", "streamlib-runtime-client-contract"),
    ("ipc", "streamlib-ipc-types"),
    ("eng", "streamlib-engine"),
    ("facade", "streamlib"),
    ("api", "streamlib-api-server"),
    ("wheel", "streamlib-python-wheel"),
];

/// A workspace of `packages`, every one a member, joined by `edges`.
fn workspace_graph_of(
    packages: &[(&str, &str)],
    edges: &[(&str, &str, &str)],
) -> NormalBuildDepGraph {
    let members: Vec<&str> = packages.iter().map(|(id, _)| *id).collect();
    NormalBuildDepGraph::from_metadata(&synthetic_metadata(&members, packages, edges)).unwrap()
}

fn displayed_engine_free_client_chains(graph: &NormalBuildDepGraph) -> Vec<String> {
    find_engine_free_client_engine_chains(graph)
        .unwrap()
        .iter()
        .map(DependencyChainToAForbiddenCrate::display_chain)
        .collect()
}

/// The tree's own shape: the engine and the api-server link the contract,
/// and nothing the clients link reaches back.
#[test]
fn clients_that_link_only_the_contract_and_the_transport_pass() {
    let graph = workspace_graph_of(
        ENGINE_FREE_CLIENT_FIXTURE_PACKAGES,
        &[
            ("cli", "contract", "normal"),
            ("cli", "ipc", "normal"),
            ("api", "contract", "normal"),
            ("api", "facade", "normal"),
            ("facade", "eng", "normal"),
            ("eng", "contract", "normal"),
            ("eng", "ipc", "normal"),
            ("wheel", "facade", "normal"),
        ],
    );

    assert_eq!(
        displayed_engine_free_client_chains(&graph),
        Vec::<String>::new()
    );
}

/// The failure names each client and the whole path to what it links, so the
/// edge to cut is on the line.
#[test]
fn a_client_reaching_an_engine_crate_through_another_crate_fails_naming_the_path() {
    let graph = workspace_graph_of(
        ENGINE_FREE_CLIENT_FIXTURE_PACKAGES,
        &[
            ("cli", "contract", "normal"),
            ("contract", "ipc", "normal"),
            ("ipc", "api", "build"),
            ("api", "facade", "normal"),
            ("facade", "eng", "normal"),
        ],
    );

    assert_eq!(
        displayed_engine_free_client_chains(&graph),
        [
            "tatolab-cli -> streamlib-runtime-client-contract -> streamlib-ipc-types -> \
             streamlib-api-server",
            "streamlib-runtime-client-contract -> streamlib-ipc-types -> streamlib-api-server",
        ]
    );
}

#[test]
fn each_engine_carrying_crate_is_refused_on_its_own() {
    for (engine_carrying_id, engine_carrying_name) in [
        ("eng", "streamlib-engine"),
        ("facade", "streamlib"),
        ("api", "streamlib-api-server"),
        ("wheel", "streamlib-python-wheel"),
    ] {
        let graph = workspace_graph_of(
            ENGINE_FREE_CLIENT_FIXTURE_PACKAGES,
            &[("contract", engine_carrying_id, "normal")],
        );

        assert_eq!(
            displayed_engine_free_client_chains(&graph),
            [format!(
                "streamlib-runtime-client-contract -> {engine_carrying_name}"
            )],
        );
    }
}

/// A test may drive the real runtime; only what links into the binary counts.
#[test]
fn an_engine_crate_reached_only_through_a_dev_edge_passes() {
    let graph = workspace_graph_of(
        ENGINE_FREE_CLIENT_FIXTURE_PACKAGES,
        &[("cli", "api", "dev"), ("contract", "eng", "dev")],
    );

    assert_eq!(
        displayed_engine_free_client_chains(&graph),
        Vec::<String>::new()
    );
}

/// A renamed crate would leave the walk nothing to start from, or nothing to
/// find, and read as a pass.
#[test]
fn a_graph_missing_a_crate_the_gate_names_is_refused_rather_than_passed() {
    for (missing_id, missing_name) in [("cli", "tatolab-cli"), ("api", "streamlib-api-server")] {
        let packages: Vec<(&str, &str)> = ENGINE_FREE_CLIENT_FIXTURE_PACKAGES
            .iter()
            .copied()
            .filter(|(id, _)| *id != missing_id)
            .collect();
        let graph = workspace_graph_of(&packages, &[]);

        let refusal = find_engine_free_client_engine_chains(&graph)
            .unwrap_err()
            .to_string();

        assert!(
            refusal.contains(&format!("no workspace crate is named `{missing_name}`")),
            "{refusal}"
        );
    }
}
