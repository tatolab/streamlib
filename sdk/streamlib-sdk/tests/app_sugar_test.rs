// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::{
    options_loading_a_stream_of_the_test_project_at,
    the_location_of_a_stream_of_the_test_project_at,
};

use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::descriptors::NodeClassImportPath;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::nodes::{Config, GeneratedNode, ManualNode, NodeSpec};
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner};
use streamlib::sdk::{App, THE_STREAM_ID_AN_APP_LOADS};

macro_rules! manual_fixture {
    ($struct_name:ident) => {
        #[streamlib::sdk::node(execution = manual)]
        struct $struct_name;
        impl ManualNode for $struct_name::Node {
            fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                Ok(())
            }
            fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                Ok(())
            }
            fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                Ok(())
            }
        }
    };
}

manual_fixture!(EquivAlpha);
manual_fixture!(EquivBeta);
manual_fixture!(PassthroughNode);
manual_fixture!(MaterializeNode);
manual_fixture!(IgnoredConnectNode);
manual_fixture!(DisplayNamedNode);

/// An engine with one empty stream loaded under the name an `App` gives its
/// own, its project in `project_directory`.
fn an_engine_loading_one_empty_stream(
    project_directory: &std::path::Path,
) -> (
    std::sync::Arc<Runner>,
    std::sync::Arc<LoadedStreamInThisRuntime>,
) {
    let runner = Runner::new().expect("runner runtime");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            THE_STREAM_ID_AN_APP_LOADS,
        ))
        .expect("the empty stream loads");
    (runner, stream)
}

fn register_session_reference<P>(registrar: &LoadedStreamInThisRuntime) -> NodeClassImportPath
where
    P: GeneratedNode + 'static,
    P::Config: Config,
{
    registrar
        .add_local::<P>(serde_json::Value::Null)
        .expect("registration must succeed")
}

/// Replace each node's minted UUID with its positional token so two graphs
/// built by identical operations differ only where the runtime minted an id.
fn normalize_node_uuids(graph_json: &serde_json::Value) -> String {
    let mut text = serde_json::to_string_pretty(graph_json).expect("graph serializes");
    for (index, node_uuid) in graph_json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .filter_map(|node| node["id"].as_str())
        .enumerate()
    {
        text = text.replace(node_uuid, &format!("<ID{index}>"));
    }
    text
}

#[test]
fn app_add_matches_the_streams_add_node_snapshot() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let (_registrar_runner, registrar) =
        an_engine_loading_one_empty_stream(project_directory.path());
    let alpha_ref = register_session_reference::<EquivAlpha::Node>(&registrar);
    let beta_ref = register_session_reference::<EquivBeta::Node>(&registrar);

    let config = serde_json::json!({});

    // App-built graph.
    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");
    app.add(alpha_ref.clone(), config.clone(), None)
        .expect("app add alpha");
    app.add(beta_ref.clone(), config.clone(), None)
        .expect("app add beta");
    let app_snapshot = normalize_node_uuids(&app.stream().to_json().expect("app graph json"));

    // Stream-built graph — the same operations, spelled out against a loaded
    // stream.
    let (_runner, stream) = an_engine_loading_one_empty_stream(project_directory.path());
    stream
        .add_node(NodeSpec::new(alpha_ref, config.clone()))
        .expect("stream add alpha");
    stream
        .add_node(NodeSpec::new(beta_ref, config))
        .expect("stream add beta");
    let stream_snapshot = normalize_node_uuids(&stream.to_json().expect("stream graph json"));

    assert_eq!(
        app_snapshot, stream_snapshot,
        "App-built and stream-built graphs must snapshot identically"
    );
}

/// `App::connect` is a faithful pass-through of its stream's `connect`: on
/// the same stream, with the same endpoints, the two calls return the
/// identical error.
/// This holds regardless of what the runtime decides (it neither swallows nor
/// rewraps the error), so it stays valid across the engine connect() fix.
#[test]
fn app_connect_is_a_faithful_passthrough_of_the_streams_connect() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};

    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");
    let node = app
        .add_local::<PassthroughNode::Node>(serde_json::json!({}), None)
        .expect("add_local returns a connectable node");

    let via_app = app.connect((&node, "no_such_out"), (&node, "no_such_in"));
    let via_stream = app.stream().connect(
        OutputLinkPortRef::new(node.node_run_id(), "no_such_out"),
        InputLinkPortRef::new(node.node_run_id(), "no_such_in"),
    );

    assert_eq!(
        format!("{:?}", via_app),
        format!("{:?}", via_stream),
        "App::connect must return exactly what the stream's connect returns"
    );
}

fn register_ported_type(short: &str, input: &str, output: &str) -> NodeClassImportPath {
    use streamlib::sdk::descriptors::{NodeClassShortName, NodeDescriptor, PortDescriptor};
    use streamlib::sdk::nodes::NODE_REGISTRY;

    let import_path = NodeClassImportPath::new(format!("{}::{short}", module_path!())).unwrap();
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short).unwrap(),
        import_path.clone(),
        "app-sugar connect test",
    )
    .with_input(PortDescriptor::new(
        input,
        "",
        false,
        streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
            streamlib::sdk::descriptors::PortFamily::Image,
        ),
    ))
    .with_output(PortDescriptor::new(
        output,
        "",
        false,
        streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
            streamlib::sdk::descriptors::PortFamily::Image,
        ),
    ));
    let _ = NODE_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

#[test]
fn app_connect_between_real_nodes_on_valid_ports_returns_ok() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let source_ref = register_ported_type("AppConnectOkSource", "_unused_in", "video");
    let sink_ref = register_ported_type("AppConnectOkSink", "video_in", "_unused_out");

    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");
    let source = app
        .add(source_ref, serde_json::json!({}), None)
        .expect("app add source");
    let sink = app
        .add(sink_ref, serde_json::json!({}), None)
        .expect("app add sink");

    // The ids `App` handed back are the default uppercase-leading form — the
    // shape that pre-fix produced `InvalidLink` for every real connect.
    assert!(source.node_run_id().to_string().starts_with('P'));
    assert!(sink.node_run_id().to_string().starts_with('P'));

    let link = app
        .connect((&source, "video"), (&sink, "video_in"))
        .expect("App::connect on valid ports must return Ok, not InvalidLink");
    assert!(
        !link.to_string().is_empty(),
        "a real connect must return a non-empty LinkUniqueId"
    );
}

#[test]
fn add_local_hello_world_materializes_a_real_node() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");

    let node = app
        .add_local::<MaterializeNode::Node>(serde_json::json!({}), None)
        .expect("add_local materializes a node");

    let graph_json = app.stream().to_json().expect("graph json");
    let node_types: Vec<&str> = graph_json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .filter_map(|n| n["type"].as_str())
        .collect();
    assert_eq!(node_types.len(), 1, "one node in the graph: {node_types:?}");
    assert!(
        node_types[0].contains("MaterializeNode"),
        "the graph's node is the add_local type: {node_types:?}"
    );

    let node_id = node.node_run_id();
    app.stream()
        .remove_node(node_id)
        .unwrap_or_else(|why| panic!("the add_local id {node_id} must name a real node: {why}"));
}

/// A config value that cannot serialize to JSON (a map with non-string keys)
/// surfaces as [`Error::Configuration`] rather than panicking — the
/// `to_config_value` error path.
#[test]
fn add_rejects_a_non_serializable_config() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    // The config is encoded before the registry is ever consulted, so any
    // path works — the serialization error must fire first.
    let reference = NodeClassImportPath::new("app_sugar_test::Whatever").unwrap();

    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");
    // A compound (tuple) map key cannot become a JSON object key — `to_value`
    // errors rather than coercing (unlike an integer key, which stringifies).
    let mut unserializable: HashMap<(i32, i32), i32> = HashMap::new();
    unserializable.insert((1, 2), 3);

    match app.add(reference, unserializable, None) {
        Err(Error::Configuration(_)) => {}
        other => panic!("expected Error::Configuration, got {other:?}"),
    }
}

#[test]
fn connect_to_nonexistent_port_surfaces_node_port_not_found() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    use streamlib::sdk::error::PortDirection;

    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");
    let node = app
        .add_local::<IgnoredConnectNode::Node>(serde_json::json!({}), None)
        .expect("add_local returns a connectable node");

    let error = app
        .connect((&node, "no_such_out"), (&node, "no_such_in"))
        .expect_err("connecting a nonexistent port must fail");

    match error {
        Error::NodePortNotFound {
            port_name,
            direction,
            ..
        } => {
            assert_eq!(port_name, "no_such_out");
            assert_eq!(direction, PortDirection::Output);
        }
        other => panic!("expected NodePortNotFound, got {other:?}"),
    }
}

/// The handle `App::add` returns reports the name the engine assigned: the
/// class's short name cast, the second of one type suffixed `-2` — learnt from
/// the handle rather than by asking the graph.
#[test]
fn the_handle_reports_the_display_name_the_engine_assigned() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let reference = register_ported_type("AppDisplayNameDefault", "_unused_in", "_unused_out");

    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");
    let first = app
        .add(reference.clone(), serde_json::json!({}), None)
        .expect("app add first");
    let second = app
        .add(reference, serde_json::json!({}), None)
        .expect("app add second");

    assert_eq!(first.display_name(), "appdisplaynamedefault");
    assert_eq!(second.display_name(), "appdisplaynamedefault-2");
}

/// A requested name reaches the graph cast, and a requested name that collides
/// is refused by name rather than suffixed — a typed name is an address.
#[test]
fn a_requested_display_name_is_cast_and_a_duplicate_is_refused() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let reference = register_ported_type("AppDisplayNameRequested", "_unused_in", "_unused_out");

    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");
    let first = app
        .add(
            reference.clone(),
            serde_json::json!({}),
            Some("Front Camera"),
        )
        .expect("app add first");
    let second = app.add(reference, serde_json::json!({}), Some("front camera"));

    assert_eq!(first.display_name(), "front-camera");
    match second {
        Err(streamlib::sdk::error::Error::NodeNameTaken { name, cast }) => {
            assert_eq!(name, "front camera");
            assert_eq!(cast, "front-camera");
        }
        other => panic!("expected NodeNameTaken, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn add_local_reaches_the_display_name_surface_too() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let app = App::new_at_stream_location(the_location_of_a_stream_of_the_test_project_at(
        project_directory.path(),
        THE_STREAM_ID_AN_APP_LOADS,
    ))
    .expect("App::new");

    let node = app
        .add_local::<DisplayNamedNode::Node>(serde_json::json!({}), Some("Blur"))
        .expect("add_local materializes a node");

    assert_eq!(node.display_name(), "blur");
}
