// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `App` is thin sugar over `Runner`: a graph built through `App` must be
//! byte-for-byte the graph built by the equivalent `Runner` calls (once the
//! nondeterministic per-node ids are normalized away), every method must be a
//! faithful pass-through that surfaces the runtime's own errors unchanged, and
//! the `add_local` hello-world path must materialize a real node with no
//! package, no `streamlib.yaml`, and no generated modules on disk.
//!
//! `App` mints default `P{cuid2}` processor ids, so `App::connect` exercises
//! the exact #1416 channel-name path the engine fix repaired: a valid link
//! between two real `add`-ed processors returns `Ok(LinkUniqueId)`, and a
//! connect to a nonexistent port surfaces the typed `ProcessorPortNotFound`
//! rather than a masking `InvalidLink`.

use std::collections::HashMap;

use streamlib::sdk::App;
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::{Config, GeneratedProcessor, ManualProcessor, ProcessorSpec, ProcessorTypeReference};
use streamlib::sdk::runtime::Runner;

// =============================================================================
// Fixtures — in-crate `#[processor]` host types. No package, no streamlib.yaml,
// no build: the macro synthesizes identity and the runtime registers them live.
// Distinct type names per test ⇒ distinct `@session/<name>` keys ⇒ the
// process-global registry never collides across the (parallel) test binary.
// =============================================================================

macro_rules! manual_fixture {
    ($struct_name:ident, $ident:literal) => {
        #[streamlib::sdk::processor($ident, execution = manual)]
        struct $struct_name;
        impl ManualProcessor for $struct_name::Processor {
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

manual_fixture!(EquivAlpha, "@tatolab/app-sugar-test/EquivAlpha");
manual_fixture!(EquivBeta, "@tatolab/app-sugar-test/EquivBeta");
manual_fixture!(PassthroughNode, "@tatolab/app-sugar-test/PassthroughNode");
manual_fixture!(MaterializeNode, "@tatolab/app-sugar-test/MaterializeNode");
manual_fixture!(IgnoredConnectNode, "@tatolab/app-sugar-test/IgnoredConnectNode");

/// Register a `#[processor]` host type live under `@session/<name>` and derive
/// the version-free reference that resolves it — the same reference shape
/// `App::add_local` builds internally, but without instantiating, so both an
/// `App` graph and a `Runner` graph can reference the one registered type.
fn register_session_reference<P>(registrar: &Runner) -> ProcessorTypeReference
where
    P: GeneratedProcessor + 'static,
    P::Config: Config,
{
    let loaded = registrar
        .add_local_blocking::<P>(serde_json::Value::Null)
        .expect("session registration must succeed");
    let descriptor = P::descriptor().expect("fixture exposes a descriptor");
    ProcessorTypeReference::ResolveToInstalled {
        org: loaded.ident.org,
        package: loaded.ident.name,
        r#type: descriptor.name.r#type,
    }
}

/// Replace each captured concrete id with its positional token so two graphs
/// built by identical operations differ only where the runtime minted a random
/// cuid2. Ids are 24-char cuid2 strings — they never alias JSON structure.
fn normalize_ids(graph_json: &serde_json::Value, ids_in_build_order: &[String]) -> String {
    let mut text = serde_json::to_string_pretty(graph_json).expect("graph serializes");
    for (index, concrete_id) in ids_in_build_order.iter().enumerate() {
        text = text.replace(concrete_id, &format!("<ID{index}>"));
    }
    text
}

/// A graph built via `App::add` and the equivalent graph built via
/// `Runner::add_processor` produce the identical snapshot once the minted ids
/// are normalized — the proof that `App::add` adds no graph shape of its own.
#[test]
fn app_add_matches_runner_add_processor_snapshot() {
    let registrar = Runner::new().expect("registrar runtime");
    let alpha_ref = register_session_reference::<EquivAlpha::Processor>(&registrar);
    let beta_ref = register_session_reference::<EquivBeta::Processor>(&registrar);

    let config = serde_json::json!({});

    // App-built graph.
    let app = App::new().expect("App::new");
    let app_alpha = app.add(alpha_ref.clone(), config.clone()).expect("app add alpha");
    let app_beta = app.add(beta_ref.clone(), config.clone()).expect("app add beta");
    let app_snapshot = normalize_ids(
        &app.runner().to_json().expect("app graph json"),
        &[app_alpha.to_string(), app_beta.to_string()],
    );

    // Runner-built graph — the same operations, spelled out against `Runner`.
    let runner = Runner::new().expect("runner runtime");
    let runner_alpha = runner
        .add_processor(ProcessorSpec::new(alpha_ref, config.clone()))
        .expect("runner add alpha");
    let runner_beta = runner
        .add_processor(ProcessorSpec::new(beta_ref, config))
        .expect("runner add beta");
    let runner_snapshot = normalize_ids(
        &runner.to_json().expect("runner graph json"),
        &[runner_alpha.to_string(), runner_beta.to_string()],
    );

    assert_eq!(
        app_snapshot, runner_snapshot,
        "App-built and Runner-built graphs must snapshot identically"
    );
}

/// `App::connect` is a faithful pass-through of `Runner::connect`: on the same
/// runner, with the same endpoints, the two calls return the identical error.
/// This holds regardless of what the runtime decides (it neither swallows nor
/// rewraps the error), so it stays valid across the engine connect() fix.
#[test]
fn app_connect_is_a_faithful_passthrough_of_runner_connect() {
    use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};

    let app = App::new().expect("App::new");
    let node = app
        .add_local::<PassthroughNode::Processor>(serde_json::json!({}))
        .expect("add_local returns a connectable id");

    let via_app = app.connect((&node, "no_such_out"), (&node, "no_such_in"));
    let via_runner = app.runner().connect(
        OutputLinkPortRef::new(&node, "no_such_out"),
        InputLinkPortRef::new(&node, "no_such_in"),
    );

    assert_eq!(
        format!("{:?}", via_app),
        format!("{:?}", via_runner),
        "App::connect must return exactly what Runner::connect returns"
    );
}

/// `App::connect_with` faithfully forwards its [`ConnectOptions`] posture to
/// `Runner::connect_with`: over a concrete producer/consumer schema mismatch,
/// `ConnectOptions::strict()` rejects at the App surface with the runtime's
/// `Error::SchemaIdentMismatch`, while the loose default over the same pair
/// still wires the link. Hardcoding the posture in `App::connect_with`
/// (strict-always or loose-always, or dropping the `options` argument) breaks
/// one half and fails here — guarding the App layer against drift from the
/// runner surface.
#[test]
fn app_connect_with_forwards_strict_posture_to_runner() {
    use streamlib::sdk::runtime::ConnectOptions;

    let (producer_ref, consumer_ref) = register_schema_mismatched_pair(
        "AppConnectWithStrictProducer",
        "AppConnectWithStrictConsumer",
    );

    let app = App::new().expect("App::new");
    let producer = app
        .add(producer_ref, serde_json::json!({}))
        .expect("app add producer");
    let consumer = app
        .add(consumer_ref, serde_json::json!({}))
        .expect("app add consumer");

    let err = app
        .connect_with(
            (&producer, "out"),
            (&consumer, "in"),
            ConnectOptions::strict(),
        )
        .expect_err("strict App::connect_with must reject the mismatched link");
    assert!(
        matches!(err, Error::SchemaIdentMismatch { .. }),
        "App::connect_with must forward the strict posture and surface \
         Error::SchemaIdentMismatch; got {err:?}"
    );

    app.connect_with(
        (&producer, "out"),
        (&consumer, "in"),
        ConnectOptions::loose(),
    )
    .expect("loose App::connect_with over the same pair must still wire the link");
}

/// Register a producer type (`out` → a `VideoFrame` schema) and a consumer type
/// (`in` → an `AudioFrame` schema) so any wired producer→consumer link is a
/// concrete schema mismatch. Unique short names per call keep the
/// process-global registry collision-free across the parallel test binary.
fn register_schema_mismatched_pair(
    producer_short: &str,
    consumer_short: &str,
) -> (ProcessorTypeReference, ProcessorTypeReference) {
    use streamlib::sdk::descriptors::{
        Org, Package, PortDescriptor, PortSchemaSpec, ProcessorDescriptor, SchemaIdent, SemVer,
        TypeName,
    };
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;

    let schema = |ty: &str| {
        PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("app-sugar-test").unwrap(),
            TypeName::new(ty).unwrap(),
            SemVer::new(1, 0, 0),
        ))
    };
    let type_id = |short: &str| {
        SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("app-sugar-test").unwrap(),
            TypeName::new(short).unwrap(),
            SemVer::new(1, 0, 0),
        )
    };

    let producer_id = type_id(producer_short);
    let producer = ProcessorDescriptor::new(producer_id.clone(), "app-sugar strict producer")
        .with_output(PortDescriptor::new("out", "", schema("VideoFrame"), false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(producer);

    let consumer_id = type_id(consumer_short);
    let consumer = ProcessorDescriptor::new(consumer_id.clone(), "app-sugar strict consumer")
        .with_input(PortDescriptor::new("in", "", schema("AudioFrame"), false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(consumer);

    (producer_id.into(), consumer_id.into())
}

/// Register a descriptor-only processor type with one real input and one real
/// output port — enough to satisfy `connect`'s port-existence check without
/// instantiating. Unique short names per call keep the process-global registry
/// collision-free across the parallel test binary.
fn register_ported_type(short: &str, input: &str, output: &str) -> ProcessorTypeReference {
    use streamlib::sdk::descriptors::{
        Org, Package, PortDescriptor, PortSchemaSpec, ProcessorDescriptor, SchemaIdent, SemVer,
        TypeName,
    };
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;

    let id = SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("app-sugar-test").unwrap(),
        TypeName::new(short).unwrap(),
        SemVer::new(1, 0, 0),
    );
    let descriptor = ProcessorDescriptor::new(id.clone(), "app-sugar connect test")
        .with_input(PortDescriptor::new(input, "", PortSchemaSpec::Any, false))
        .with_output(PortDescriptor::new(output, "", PortSchemaSpec::Any, false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    id.into()
}

/// End-to-end through the `App` surface: `App::add` mints default `P{cuid2}`
/// ids, and `App::connect` between two real added processors on valid ports
/// returns `Ok(LinkUniqueId)` — the exact #1416 regression path (channel-name
/// derivation lowercases the uppercase-leading id) exercised through the sugar.
#[test]
fn app_connect_between_real_processors_on_valid_ports_returns_ok() {
    let source_ref = register_ported_type("AppConnectOkSource", "_unused_in", "video");
    let sink_ref = register_ported_type("AppConnectOkSink", "video_in", "_unused_out");

    let app = App::new().expect("App::new");
    let source = app.add(source_ref, serde_json::json!({})).expect("app add source");
    let sink = app.add(sink_ref, serde_json::json!({})).expect("app add sink");

    // The ids `App` handed back are the default uppercase-leading form — the
    // shape that pre-fix produced `InvalidLink` for every real connect.
    assert!(source.to_string().starts_with('P'));
    assert!(sink.to_string().starts_with('P'));

    let link = app
        .connect((&source, "video"), (&sink, "video_in"))
        .expect("App::connect on valid ports must return Ok, not InvalidLink");
    assert!(
        !link.to_string().is_empty(),
        "a real connect must return a non-empty LinkUniqueId"
    );
}

/// The hello-world path: `add_local` registers an in-crate `#[processor]` type
/// with no package / `streamlib.yaml` / generated modules on disk, and returns
/// a [`ProcessorUniqueId`] that is a real, addressable node in the graph.
#[test]
fn add_local_hello_world_materializes_a_real_node() {
    let app = App::new().expect("App::new");

    let node = app
        .add_local::<MaterializeNode::Processor>(serde_json::json!({}))
        .expect("add_local materializes a node");

    let graph_json = app.runner().to_json().expect("graph json");
    let node_ids: Vec<&str> = graph_json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .filter_map(|n| n["id"].as_str())
        .collect();

    assert!(
        node_ids.contains(&node.to_string().as_str()),
        "the add_local id {node} must name a real node in the graph, found {node_ids:?}"
    );
}

/// A config value that cannot serialize to JSON (a map with non-string keys)
/// surfaces as [`Error::Configuration`] rather than panicking — the
/// `to_config_value` error path.
#[test]
fn add_rejects_a_non_serializable_config() {
    use streamlib::sdk::descriptors::{Org, Package, TypeName};

    // The config is encoded before the reference is ever resolved, so any
    // reference works — the serialization error must fire first.
    let reference = ProcessorTypeReference::ResolveToInstalled {
        org: Org::new("tatolab").unwrap(),
        package: Package::new("app-sugar-test").unwrap(),
        r#type: TypeName::new("Whatever").unwrap(),
    };

    let app = App::new().expect("App::new");
    // A compound (tuple) map key cannot become a JSON object key — `to_value`
    // errors rather than coercing (unlike an integer key, which stringifies).
    let mut unserializable: HashMap<(i32, i32), i32> = HashMap::new();
    unserializable.insert((1, 2), 3);

    match app.add(reference, unserializable) {
        Err(Error::Configuration(_)) => {}
        other => panic!("expected Error::Configuration, got {other:?}"),
    }
}

/// Connecting a nonexistent port on a real (default `P{cuid2}`-id) processor
/// surfaces the runtime's typed [`Error::ProcessorPortNotFound`] through
/// `App::connect` unchanged — never a masking `InvalidLink` from the
/// channel-name grammar.
#[test]
fn connect_to_nonexistent_port_surfaces_processor_port_not_found() {
    use streamlib::sdk::error::PortDirection;

    let app = App::new().expect("App::new");
    let node = app
        .add_local::<IgnoredConnectNode::Processor>(serde_json::json!({}))
        .expect("add_local returns a connectable id");

    let error = app
        .connect((&node, "no_such_out"), (&node, "no_such_in"))
        .expect_err("connecting a nonexistent port must fail");

    match error {
        Error::ProcessorPortNotFound { port_name, direction, .. } => {
            assert_eq!(port_name, "no_such_out");
            assert_eq!(direction, PortDirection::Output);
        }
        other => panic!("expected ProcessorPortNotFound, got {other:?}"),
    }
}
