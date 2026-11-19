// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `check_link` over a real engine answers what `connect` answers for the same link: the same
//! whole refusal for each link `connect` refuses, and the outcome of each link it makes, while
//! the graph, its document and the project's history are left as they were.

use streamlib::sdk::descriptors::{
    AudioWindowContract, FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeKind,
    PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
};

use super::input_value_set_tests::{MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer};
use super::*;

/// The UUID of the top-level `GraphInput` `volume` beside the mixer, whose `value` is a setting.
const VOLUME_GRAPH_INPUT_NODE_UUID: &str = "71717171-7171-4171-8171-717171717171";

/// The UUID no node of the mixer's stream holds.
const NO_NODE_UUID: &str = "0e0e0e0e-0e0e-4e0e-8e0e-0e0e0e0e0e0e";

/// A project whose stream `main` holds the mixer, its `monitor`, and a top-level `GraphInput`
/// `volume`.
fn a_project_whose_main_stream_holds_a_mixer_and_a_graph_input() -> ProjectWithGraphDocuments {
    let project = a_project_whose_main_stream_holds_a_mixer();
    let mut graph = project.the_graph_document_of("main");
    graph["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": VOLUME_GRAPH_INPUT_NODE_UUID, "name": "volume", "type": "tatolab.stream:GraphInput",
        "config": { "port": "volume", "family": "channels" },
    }));
    project.write_the_graph_document(graph);
    project
}

/// The link from `source` to `target`, each `(node, port)`, in `stream`, as `check_link` takes it.
fn the_link(stream: Value, source: (&str, &str), target: (&str, &str)) -> Value {
    json!({
        "stream": stream,
        "source": { "node": source.0, "port": source.1 },
        "target": { "node": target.0, "port": target.1 },
    })
}

/// `link` as `connect` takes it, by the agent `alice`.
fn connected_by_alice(link: &Value) -> Value {
    let mut connect = link.clone();
    connect["author"] = an_agent_named("alice");
    connect
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// `check_link`'s answer for `link`; a refusal of `check_link` itself fails the test.
    async fn the_check_of(&self, link: &Value) -> Value {
        self.the_answer_or_the_refusal_of("check_link", link.clone())
            .await
            .unwrap_or_else(|refusal| panic!("check_link answers {link}: {refusal:?}"))
    }
}

/// Each link `connect` refuses, `check_link` answers `refused` with the very refusal `connect`
/// answers for it, and the graph document is left as it was by the check.
///
/// Mental-revert: a check that judged pin types but read a node or a stream its own way would
/// answer another code, place or words than `connect` for one of these links.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_the_refusal_connect_answers_for_each_link_it_refuses() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer_and_a_graph_input();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let links_connect_refuses = [
        (
            "pin types that do not connect",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (MIXER_NODE_UUID, "video"),
            ),
        ),
        (
            "a setting",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (VOLUME_GRAPH_INPUT_NODE_UUID, "value"),
            ),
        ),
        (
            "a typed output into an untyped input whose family refuses it",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (SOURCE_NODE_UUID, "video"),
            ),
        ),
        (
            "a port the node does not have",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (MIXER_NODE_UUID, "nothing"),
            ),
        ),
        (
            "a node the stream does not hold",
            the_link(
                stream.clone(),
                (MIXER_NODE_UUID, "level"),
                (NO_NODE_UUID, "gain"),
            ),
        ),
        (
            "a node named by no UUID",
            the_link(
                stream.clone(),
                ("mixer", "level"),
                (MIXER_NODE_UUID, "gain"),
            ),
        ),
        (
            "a stream the runtime does not hold",
            the_link(
                json!("no-such-stream-0a0a0a"),
                (MIXER_NODE_UUID, "level"),
                (MIXER_NODE_UUID, "gain"),
            ),
        ),
    ];
    let saved_before = project.the_graph_document_of("main");

    for (what, link) in links_connect_refuses {
        let checked = engine.the_check_of(&link).await;
        assert_eq!(
            project.the_graph_document_of("main"),
            saved_before,
            "{what}"
        );
        let refused = engine
            .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
            .await
            .expect_err(what);

        assert_eq!(checked["outcome"], "refused", "{what}: {checked}");
        assert_eq!(
            checked["refusal"],
            serde_json::to_value(&*refused).unwrap(),
            "{what}: check_link answers the refusal connect answers"
        );
        assert!(checked.get("convert_node").is_none(), "{what}: {checked}");
    }
}

/// A link `connect` makes, `check_link` answers `connects`, with no refusal, and leaves the live
/// graph, its document and the project's history as they were; once `connect` has made it, a
/// second check answers the refusal a second `connect` answers.
///
/// Mental-revert: a check applying the link and reversing it would leave a commit for `undo`.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_a_link_connect_makes_and_changes_nothing() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let link = the_link(
        json!(project.stream_asset_of("main")),
        (MIXER_NODE_UUID, "level"),
        (MIXER_NODE_UUID, "gain"),
    );
    let saved_before = project.the_graph_document_of("main");

    let checked = engine.the_check_of(&link).await;

    assert_eq!(checked, json!({ "outcome": "connects" }));
    assert_eq!(project.the_graph_document_of("main"), saved_before);
    assert_eq!(
        engine.the_live_graph_of(&stream_id)["links"],
        json!([]),
        "the check makes no link"
    );
    let undo = json!({ "project_directory": project.path(), "author": an_agent_named("alice") });
    let nothing_to_undo = engine
        .the_answer_or_the_refusal_of("undo", undo)
        .await
        .expect_err("the check records nothing in history");
    assert!(
        nothing_to_undo
            .runtime_words
            .contains("no transaction left to undo"),
        "{nothing_to_undo:?}"
    );

    engine
        .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
        .await
        .expect("connect makes the link the check answered");
    let checked_again = engine.the_check_of(&link).await;
    let refused_again = engine
        .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
        .await
        .expect_err("the graph already holds the link");

    assert_eq!(checked_again["outcome"], "refused", "{checked_again}");
    assert_eq!(
        checked_again["refusal"],
        serde_json::to_value(&*refused_again).unwrap()
    );
}

/// A link inside a placed stream, under a non-empty `level`, is checked as `connect` judges it
/// there: each link `connect` refuses inside is answered with the very refusal it answers, and
/// one it makes with `connects`, the placed stream's document left as it was by the check.
///
/// Mental-revert: refusing a non-empty `level` unchecked fails `check_link` itself.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_what_connect_answers_for_a_link_inside_a_placed_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let inside = engine
        .the_mixer_collapsed_into_a_placed_stream(&project)
        .await;
    let link_inside = |source: (&str, &str), target: (&str, &str)| {
        let mut link = the_link(json!(project.stream_asset_of("main")), source, target);
        link["level"] = json!([inside.subgraph]);
        link
    };
    let sub_before = project.the_graph_document_of("sub");

    for (what, link) in [
        (
            "pin types that do not connect",
            link_inside((&inside.mixer, "level"), (&inside.mixer, "video")),
        ),
        (
            "a setting",
            link_inside((&inside.mixer, "level"), (&inside.volume, "value")),
        ),
        (
            "a node the placed stream does not hold",
            link_inside((&inside.mixer, "level"), (NO_NODE_UUID, "gain")),
        ),
    ] {
        let checked = engine.the_check_of(&link).await;
        assert_eq!(project.the_graph_document_of("sub"), sub_before, "{what}");
        let refused = engine
            .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
            .await
            .expect_err(what);

        assert_eq!(checked["outcome"], "refused", "{what}: {checked}");
        assert_eq!(
            checked["refusal"],
            serde_json::to_value(&*refused).unwrap(),
            "{what}: check_link answers the refusal connect answers"
        );
    }

    let made = link_inside((&inside.mixer, "level"), (&inside.mixer, "gain"));
    assert_eq!(
        engine.the_check_of(&made).await,
        json!({ "outcome": "connects" })
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before);
}

/// The function node type `node_type`, taking `inputs`, each a Records input or, with an audio
/// window, a Channels one declaring it, and giving `outputs`, each of its family.
fn a_function_node_type(
    node_type: &str,
    inputs: &[(&str, Option<AudioWindowContract>)],
    outputs: &[(&str, PortFamily)],
) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type.clone(),
        "a function node type a link check judges",
    );
    descriptor.node_kind = NodeKind::Function {
        language: FunctionNodeLanguage::Python,
    };
    for (input, audio_window) in inputs {
        let family = match audio_window {
            Some(_) => PortFamily::Channels,
            None => PortFamily::Records,
        };
        let mut port =
            PortDescriptor::new(*input, "", true, PortFamilyDeclaration::of_family(family))
                .with_delivery_profile("newest");
        port.audio_window = audio_window.clone();
        descriptor = descriptor.with_input(port);
    }
    for (output, family) in outputs {
        descriptor = descriptor.with_output(PortDescriptor::new(
            *output,
            "",
            true,
            PortFamilyDeclaration::of_family(*family),
        ));
    }
    descriptor
}

const LFO_TYPE: &str = "link_check_test:Lfo";
const WAVE_TYPE: &str = "link_check_test:Wave";
const MIX_TYPE: &str = "link_check_test:Mix";
const REMAP_TYPE: &str = "link_check_test:Remap";
const WINDOWED_TYPE: &str = "link_check_test:Windowed";
const SECONDS_TYPE: &str = "link_check_test:Seconds";
const FRAMES_TYPE: &str = "link_check_test:Frames";

const FIRST_LFO_UUID: &str = "1f1f1f1f-1f1f-4f1f-8f1f-1f1f1f1f1f1f";
const SECOND_LFO_UUID: &str = "2f2f2f2f-2f2f-4f2f-8f2f-2f2f2f2f2f2f";
const WAVE_UUID: &str = "9f9f9f9f-9f9f-4f9f-8f9f-9f9f9f9f9f9f";
const MIX_UUID: &str = "3f3f3f3f-3f3f-4f3f-8f3f-3f3f3f3f3f3f";
const REMAP_UUID: &str = "4f4f4f4f-4f4f-4f4f-8f4f-4f4f4f4f4f4f";
const WINDOWED_UUID: &str = "5f5f5f5f-5f5f-4f5f-8f5f-5f5f5f5f5f5f";
const SECONDS_UUID: &str = "6f6f6f6f-6f6f-4f6f-8f6f-6f6f6f6f6f6f";
const FRAMES_UUID: &str = "7f7f7f7f-7f7f-4f7f-8f7f-7f7f7f7f7f7f";

/// The node type `node_type` whose one port, `port`, is typed `pin_type` and means `meaning`:
/// an output, or, with `is_an_input`, an input.
fn a_node_type_with_a_port_meaning(
    node_type: &str,
    port: &str,
    is_an_input: bool,
    pin_type: PinType,
    meaning: &str,
) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    let mut pin = PinDeclaration::exactly(pin_type.clone());
    pin.annotations.meaning = Some(meaning.to_owned());
    let port = PortDescriptor::new(
        port,
        "",
        true,
        PortFamilyDeclaration::of_family(
            PortFamily::carrying(&pin_type).unwrap_or(PortFamily::Records),
        ),
    )
    .with_delivery_profile("newest")
    .with_pin_declaration(pin);
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type,
        "a node type whose port states a meaning",
    );
    if is_an_input {
        descriptor.with_input(port)
    } else {
        descriptor.with_output(port)
    }
}

/// Register each node type the link check tests below place, once per test binary, a
/// `PreviousValue` node type, which a project's interpreter describes, among them.
fn register_the_link_check_node_types_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        for descriptor in [
            a_function_node_type(LFO_TYPE, &[], &[("result", PortFamily::Records)]),
            a_function_node_type(
                streamlib_project_repository::PREVIOUS_VALUE_NODE_TYPE,
                &[(
                    streamlib_project_repository::PREVIOUS_VALUE_NODE_INPUT_PORT,
                    None,
                )],
                &[(
                    streamlib_project_repository::PREVIOUS_VALUE_NODE_OUTPUT_PORT,
                    PortFamily::Records,
                )],
            ),
            a_function_node_type(WAVE_TYPE, &[], &[("result", PortFamily::Channels)]),
            a_function_node_type(
                MIX_TYPE,
                &[("a", None), ("b", None)],
                &[("result", PortFamily::Records)],
            ),
            a_function_node_type(
                REMAP_TYPE,
                &[("x", None)],
                &[("result", PortFamily::Records)],
            ),
            a_function_node_type(
                WINDOWED_TYPE,
                &[("audio", Some(AudioWindowContract::MatchDevice {}))],
                &[],
            ),
            a_node_type_with_a_port_meaning(
                SECONDS_TYPE,
                "elapsed",
                false,
                PinType::Float { exact_size: None },
                "seconds",
            ),
            a_node_type_with_a_port_meaning(
                FRAMES_TYPE,
                "count",
                true,
                PinType::Integer { exact_size: None },
                "frames",
            ),
        ] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("each link check node type is registered here alone");
        }
    });
}

/// A project whose stream `main` holds two Lfos, a Wave giving Channels, a Mix the first feeds on `a`, a Remap the Mix
/// feeds, a function node whose `audio` input declares an audio window, a node giving Float
/// `seconds` and one taking Integer `frames`.
fn a_project_whose_main_stream_holds_function_nodes() -> ProjectWithGraphDocuments {
    register_the_link_check_node_types_once();
    let node = |id: &str, name: &str, node_type: &str| json!({ "id": id, "name": name, "type": node_type, "config": {} });
    let link = |source: (&str, &str), target: (&str, &str)| {
        json!({ "source": { "node": source.0, "port": source.1 },
                "target": { "node": target.0, "port": target.1 } })
    };
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            node(FIRST_LFO_UUID, "first_lfo", LFO_TYPE),
            node(SECOND_LFO_UUID, "second_lfo", LFO_TYPE),
            node(WAVE_UUID, "wave", WAVE_TYPE),
            node(MIX_UUID, "mix", MIX_TYPE),
            node(REMAP_UUID, "remap", REMAP_TYPE),
            node(WINDOWED_UUID, "windowed", WINDOWED_TYPE),
            node(SECONDS_UUID, "clock", SECONDS_TYPE),
            node(FRAMES_UUID, "counter", FRAMES_TYPE),
        ],
        "links": [
            link((FIRST_LFO_UUID, "result"), (MIX_UUID, "a")),
            link((MIX_UUID, "result"), (REMAP_UUID, "x")),
        ],
    }))
}

/// Each link an island cannot take, `check_link` answers `refused` with the very refusal
/// `connect` answers for it, and the check leaves the graph document as it was.
///
/// Mental-revert: a check that judged the link without the island's own refusals would answer
/// `connects` for these links, which `connect` refuses.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_the_refusal_connect_answers_for_each_link_an_island_cannot_take() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = json!(project.stream_asset_of("main"));
    let links_an_island_cannot_take = [
        (
            "a second link into a port an island feeds",
            the_link(stream.clone(), (SECOND_LFO_UUID, "result"), (MIX_UUID, "a")),
            "shares the port with no other link",
        ),
        (
            "an island into a port declaring an audio window",
            the_link(
                stream.clone(),
                (WAVE_UUID, "result"),
                (WINDOWED_UUID, "audio"),
            ),
            "declares an `audio_window`",
        ),
    ];
    let saved_before = project.the_graph_document_of("main");

    for (what, link, words) in links_an_island_cannot_take {
        let checked = engine.the_check_of(&link).await;
        assert_eq!(
            project.the_graph_document_of("main"),
            saved_before,
            "{what}"
        );
        let refused = engine
            .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
            .await
            .expect_err(what);

        assert_eq!(checked["outcome"], "refused", "{what}: {checked}");
        assert_eq!(
            checked["refusal"],
            serde_json::to_value(&*refused).unwrap(),
            "{what}: check_link answers the refusal connect answers"
        );
        assert!(refused.runtime_words.contains(words), "{what}: {refused:?}");
    }
}

/// A link closing a loop of function nodes no `PreviousValue` node is in, which `connect`
/// closes through a `PreviousValue` node it adds, is refused unchecked, and the graph is left as
/// it was.
///
/// Mental-revert: a check judging the drawn link alone would answer what the engine makes of a
/// link `connect` never makes.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_refuses_unchecked_a_link_connect_closes_through_a_previous_value_node() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let links_before = engine.the_live_graph_of(&stream_id)["links"].clone();
    let closing_the_loop = the_link(
        json!(project.stream_asset_of("main")),
        (REMAP_UUID, "result"),
        (MIX_UUID, "b"),
    );

    let refusal = engine
        .the_answer_or_the_refusal_of("check_link", closing_the_loop)
        .await
        .expect_err("a link connect closes through a PreviousValue node is not checked");

    assert_eq!(refusal.action, "check_link", "{refusal:?}");
    assert!(
        refusal.runtime_words.contains("`PreviousValue` node")
            && refusal.runtime_words.contains("not checked yet"),
        "{refusal:?}"
    );
    assert_eq!(engine.the_live_graph_of(&stream_id)["links"], links_before);
}

/// A link closing a loop of function nodes inside a placed stream, which `connect` closes
/// through a `PreviousValue` node it adds, has each of that node's two links judged as the top
/// level judges them: a second link into a port an island already feeds is refused there as it
/// is at the stream's own level, and the placed stream's document is left as it was.
///
/// Mental-revert: judging only a plain connect at a placed level saves the `PreviousValue`
/// node and its links.
#[tokio::test(flavor = "multi_thread")]
async fn a_loop_closed_inside_a_placed_stream_has_its_previous_value_links_judged() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let refused_at_the_top = engine
        .the_answer_or_the_refusal_of(
            "connect",
            connected_by_alice(&the_link(
                json!(stream_asset),
                (REMAP_UUID, "result"),
                (MIX_UUID, "a"),
            )),
        )
        .await
        .expect_err("the stream's own level refuses a second link into `mix.a`");
    let collapsed = engine
        .the_answer_or_the_refusal_of(
            "collapse_selection_into_subgraph",
            json!({ "stream": stream_asset, "nodes": [FIRST_LFO_UUID, MIX_UUID, REMAP_UUID],
                    "name": "sub", "author": an_agent_named("alice") }),
        )
        .await
        .expect("the function nodes collapse into `sub`");
    let subgraph = collapsed["added_nodes"][0]["node"]
        .as_str()
        .expect("the placing node")
        .to_owned();
    let sub_before = project.the_graph_document_of("sub");
    let drawn = |name: &str| {
        the_uuid_a_held_node_takes_under(
            &subgraph,
            the_node_named(&sub_before, name)["id"]
                .as_str()
                .expect("the node's UUID"),
        )
    };
    let mut closing_the_loop_inside = the_link(
        json!(stream_asset),
        (&drawn("remap"), "result"),
        (&drawn("mix"), "a"),
    );
    closing_the_loop_inside["level"] = json!([subgraph]);

    let refused_inside = engine
        .the_answer_or_the_refusal_of("connect", connected_by_alice(&closing_the_loop_inside))
        .await
        .expect_err("a second link into `mix.a` is refused inside `sub` too");

    for refused in [&refused_at_the_top, &refused_inside] {
        assert!(
            refused
                .runtime_words
                .contains("shares the port with no other link"),
            "{refused:?}"
        );
    }
    assert_eq!(refused_inside.refusal_code, refused_at_the_top.refusal_code);
    assert_eq!(project.the_graph_document_of("sub"), sub_before);
}

/// A Float output into an Integer input, each stating another meaning, connects for free with a
/// warning that the meanings differ, in their wire spellings.
///
/// Mental-revert: a check answering the outcome without the verdict's warnings would answer no
/// `warnings`.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_answers_a_free_conversion_and_the_warning_the_verdict_gives() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let checked = engine
        .the_check_of(&the_link(
            json!(project.stream_asset_of("main")),
            (SECONDS_UUID, "elapsed"),
            (FRAMES_UUID, "count"),
        ))
        .await;

    assert_eq!(
        checked,
        json!({
            "outcome": "connects_with_free_conversion",
            "warnings": [{
                "warning": "meanings_differ",
                "output_meaning": "seconds",
                "input_meaning": "frames",
            }],
        })
    );
}

/// A link reaching a fragment node, which runs as no node of its own, is refused unchecked.
#[tokio::test(flavor = "multi_thread")]
async fn check_link_refuses_unchecked_a_link_reaching_a_fragment_node() {
    const FRAGMENT_NODE_UUID: &str = "8f8f8f8f-8f8f-4f8f-8f8f-8f8f8f8f8f8f";
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    project.write_the_graph_document(json!({
        "stream": "left",
        "nodes": [
            { "id": FRAGMENT_NODE_UUID, "name": "main", "type": STREAM_FRAGMENT_NODE_TYPE,
              "config": { "stream": { "asset": project.stream_asset_of("main") } } },
            { "id": SOURCE_NODE_UUID, "name": "monitor",
              "type": StreamToolsTestSink::node_class_import_path().as_str(), "config": {} },
        ],
        "links": [],
    }));
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    engine.a_stream_loaded_from_its_graph_document(&project, "left");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "check_link",
            the_link(
                json!(project.stream_asset_of("left")),
                (FRAGMENT_NODE_UUID, "video"),
                (SOURCE_NODE_UUID, "video"),
            ),
        )
        .await
        .expect_err("a link reaching a fragment node is not checked");

    assert_eq!(refusal.action, "check_link", "{refusal:?}");
    assert!(
        refusal.runtime_words.contains("fragment")
            && refusal.runtime_words.contains("not checked yet"),
        "{refusal:?}"
    );
}

/// A saved graph holding a second link into a port an island feeds loads into a stopped stream,
/// as a saved graph's link is judged leniently so the graph opens, while a fresh `connect` of
/// that same link, once it is taken out, is refused.
///
/// Mental-revert: a load judging a saved graph's link as one made now would refuse to open the
/// graph.
#[tokio::test(flavor = "multi_thread")]
async fn a_saved_graph_an_island_cannot_take_loads_while_a_fresh_connect_of_its_link_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_function_nodes();
    let mut graph = project.the_graph_document_of("main");
    let second_link_into_the_mix = json!({
        "source": { "node": SECOND_LFO_UUID, "port": "result" },
        "target": { "node": MIX_UUID, "port": "a" },
    });
    graph["links"]
        .as_array_mut()
        .expect("links")
        .push(second_link_into_the_mix);
    project.write_the_graph_document(graph);

    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");

    assert_eq!(
        engine.the_live_graph_of(&stream_id)["links"]
            .as_array()
            .map(Vec::len),
        Some(3),
        "the saved graph opens with each link it holds"
    );
    let link = the_link(
        json!(project.stream_asset_of("main")),
        (SECOND_LFO_UUID, "result"),
        (MIX_UUID, "a"),
    );
    engine
        .the_answer_or_the_refusal_of("disconnect", connected_by_alice(&link))
        .await
        .expect("the saved link is taken out");
    let refused = engine
        .the_answer_or_the_refusal_of("connect", connected_by_alice(&link))
        .await
        .expect_err("a fresh connect of the link is refused");
    assert!(
        refused
            .runtime_words
            .contains("shares the port with no other link"),
        "{refused:?}"
    );
}
