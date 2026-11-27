// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node of a conversion whose output follows a setting carries the type it gives under the
//! settings it holds into every judgement of a link: one past a node whose output follows its
//! input, a wired link past the input it feeds, and a setting changed while it is wired, which
//! marks a wired link it no longer fits as differing rather than being refused.

use serde_json::{Map, json};

use super::*;
use crate::core::descriptors::{
    DeclaredPinType, PinConversionOutputPick, PinConversionOutputPicked,
};
use crate::core::graph::{Graph, GraphEdgeWithComponents, LinkPinTypesDifferComponent};

const RASTER_SOURCE: &str = "VerdictCheckRasterSource";
const RASTER_TO_TEXTURE: &str = "VerdictCheckRasterToTexture";
const TEXTURE_FOLLOWING_ITS_INPUT: &str = "VerdictCheckTextureFollowingItsInput";
const BGRA_TEXTURE_SINK: &str = "VerdictCheckBgraTextureSink";
const RGBA_ONLY_TEXTURE_SINK: &str = "VerdictCheckRgbaOnlyTextureSink";

fn raster() -> PinType {
    PinType::Named {
        named_pin_type: NamedPinTypeImportPath::new("verdict_check:Raster").unwrap(),
    }
}

fn a_texture_of(format: TexturePixelFormat) -> PinType {
    PinType::Texture {
        pixel_formats: vec![format],
    }
}

/// The two formats a `RASTER_TO_TEXTURE` node writes, `format` naming one.
fn a_format_it_writes(wire_name: &str) -> Option<TexturePixelFormat> {
    TexturePixelFormat::from_wire_name(wire_name).filter(|format| {
        matches!(
            format,
            TexturePixelFormat::Rgba8Unorm | TexturePixelFormat::Bgra8Unorm
        )
    })
}

/// The first format it writes that `input` takes, `rgba8_unorm` for an input taking any.
fn the_format_picked_for(input: &PinType) -> Option<PinConversionOutputPicked> {
    let PinType::Texture { pixel_formats } = input else {
        return None;
    };
    let format = if pixel_formats.is_empty() {
        TexturePixelFormat::Rgba8Unorm
    } else {
        pixel_formats
            .iter()
            .copied()
            .find(|format| a_format_it_writes(format.wire_name()).is_some())?
    };
    Some(PinConversionOutputPicked {
        input_values: Map::from_iter([("format".to_owned(), json!(format.wire_name()))]),
        output_pin_type: a_texture_of(format),
    })
}

/// The Texture of the `format` held, `rgba8_unorm` when none is.
fn the_texture_given_under(held: &Map<String, Value>) -> Option<PinType> {
    match held.get("format") {
        None => Some(a_texture_of(TexturePixelFormat::Rgba8Unorm)),
        Some(format) => a_format_it_writes(format.as_str()?).map(a_texture_of),
    }
}

fn register_the_held_settings_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        use TexturePixelFormat::{Bgra8Unorm, Rgba8Unorm};
        register(a_node_type(
            class_path(RASTER_SOURCE),
            Vec::new(),
            vec![a_typed_port("raster", raster())],
        ));
        NODE_REGISTRY
            .register_a_conversion_picking_for_a_test(
                a_node_type(
                    NodeClassImportPath::of_built_in_node(RASTER_TO_TEXTURE),
                    vec![a_typed_port("raster", raster())],
                    vec![a_texture_port("image", &[])],
                )
                .stating_a_conversion()
                .with_config_schema(json!({
                    "type": "object",
                    "properties": {"format": {"type": "string"}},
                })),
                Box::new(|_node| Err(Error::NotSupported("a fixture runs no node".to_owned()))),
                PinConversionOutputPick {
                    picked_for: the_format_picked_for,
                    given_under: the_texture_given_under,
                },
            )
            .expect("register the fixture conversion");
        register(a_node_type(
            class_path(TEXTURE_FOLLOWING_ITS_INPUT),
            vec![a_texture_port("in", &[])],
            vec![
                a_texture_port("out", &[]).with_pin_declaration(PinDeclaration {
                    pin_type: DeclaredPinType::SameAsInput {
                        input_port_name: "in".to_owned(),
                    },
                    ..PinDeclaration::exactly(PinType::Texture {
                        pixel_formats: Vec::new(),
                    })
                }),
            ],
        ));
        register(a_node_type(
            class_path(BGRA_TEXTURE_SINK),
            vec![a_texture_port("video", &[Bgra8Unorm])],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(RGBA_ONLY_TEXTURE_SINK),
            vec![a_texture_port("video", &[Rgba8Unorm])],
            Vec::new(),
        ));
    });
}

/// The nodes each test links: a convert node holding `config`, a node whose output follows its
/// input, and a sink of each format the convert node writes.
struct HeldSettingsFixture {
    compiler: Arc<Compiler>,
    convert_node: NodeRunId,
    following: NodeRunId,
    bgra_sink: NodeRunId,
    rgba_sink: NodeRunId,
}

impl HeldSettingsFixture {
    fn with_the_convert_node_holding(config: Value) -> Self {
        register_the_verdict_fixture_node_types();
        register_the_held_settings_fixture_node_types();
        let compiler = Arc::new(Compiler::new());
        let convert_node = a_node_configured(
            &compiler,
            NodeClassImportPath::of_built_in_node(RASTER_TO_TEXTURE),
            config,
        );
        let following = a_node_of(&compiler, class_path(TEXTURE_FOLLOWING_ITS_INPUT));
        let bgra_sink = a_node_of(&compiler, class_path(BGRA_TEXTURE_SINK));
        let rgba_sink = a_node_of(&compiler, class_path(RGBA_ONLY_TEXTURE_SINK));
        Self {
            compiler,
            convert_node,
            following,
            bgra_sink,
            rgba_sink,
        }
    }

    fn connect(&self, from: (&NodeRunId, &str), to: (&NodeRunId, &str)) -> Result<()> {
        connect_on_this_thread(
            &self.compiler,
            OutputLinkPortRef::new(from.0.clone(), from.1),
            InputLinkPortRef::new(to.0.clone(), to.1),
        )
        .map(drop)
    }
}

/// A link out of a node whose output follows its input, fed by a convert node holding no
/// `format`, is judged by the `rgba8_unorm` that node writes: refused into an input taking only
/// `bgra8_unorm`, made into one taking `rgba8_unorm`.
///
/// Mental-revert: resolving the following output as the Texture of no format the convert node
/// states connects it to the `bgra8_unorm` input with a warning.
#[test]
fn a_link_past_a_node_following_a_convert_node_is_judged_by_the_type_it_holds() {
    let fixture = HeldSettingsFixture::with_the_convert_node_holding(Value::Null);
    fixture
        .connect((&fixture.convert_node, "image"), (&fixture.following, "in"))
        .expect("an input taking any Texture takes rgba8_unorm");

    let refused = fixture
        .connect((&fixture.following, "out"), (&fixture.bgra_sink, "video"))
        .expect_err("the convert node writes rgba8_unorm");
    let words = the_verdicts_refusal(refused).to_string();
    assert!(words.contains("rgba8_unorm"), "{words}");
    fixture
        .connect((&fixture.following, "out"), (&fixture.rgba_sink, "video"))
        .expect("rgba8_unorm is what the input takes");
}

/// A convert node linked into a node whose output is already wired on is judged, past that
/// input, by the format it holds: `rgba8_unorm` is refused by the wired `bgra8_unorm` input,
/// `bgra8_unorm` is taken.
///
/// Mental-revert: judging the wired link past the input by the Texture of no format the convert
/// node states takes the link with a warning.
#[test]
fn a_convert_node_feeding_a_wired_on_input_is_judged_by_the_type_it_holds() {
    for (config, taken) in [
        (Value::Null, false),
        (json!({ "format": "bgra8_unorm" }), true),
    ] {
        let fixture = HeldSettingsFixture::with_the_convert_node_holding(config.clone());
        fixture
            .connect((&fixture.following, "out"), (&fixture.bgra_sink, "video"))
            .expect("a Texture of no format reaches the input with a warning");

        let linked = fixture.connect((&fixture.convert_node, "image"), (&fixture.following, "in"));
        match (linked, taken) {
            (Ok(()), true) => {}
            (Err(Error::InvalidLink(words)), false) => {
                assert!(words.contains("bgra8_unorm"), "{words}");
            }
            (other, _) => panic!("{config}: {other:?}"),
        }
    }
}

/// Set the convert node's `format` to `format` in `graph` and answer the words of each link
/// judged again as differing, the links of the convert node's type choices reach.
fn set_the_format_and_judge(
    graph: &mut Graph,
    convert_node: &NodeRunId,
    format: &str,
) -> Vec<String> {
    graph
        .traversal_mut()
        .v(convert_node)
        .first_mut()
        .expect("the node is held")
        .input_values
        .insert("format".to_owned(), json!(format));
    let judged_again = graph.judge_again_each_link_the_type_choices_of_the_nodes_reach(
        std::slice::from_ref(convert_node),
    );
    graph.mark_each_link_judged_again(&judged_again);
    judged_again
        .differing
        .into_iter()
        .map(|(_, differing)| {
            format!(
                "{} / {}: {}",
                differing.output_pin_type, differing.input_pin_type, differing.why
            )
        })
        .collect()
}

/// A `format` set on the convert node that a wired link out of it, or past the input that link
/// feeds, does not take marks that link as differing, naming the type it would then carry; one
/// each link takes again takes the mark off. The change itself is never refused.
///
/// Mental-revert: judging only the outputs a setting types by their declaration marks nothing,
/// since the convert node's output declares one Texture whatever it holds.
#[test]
fn a_format_a_wired_link_does_not_take_marks_the_link_differing_until_it_takes_it_again() {
    for through_a_following_node in [false, true] {
        let fixture = HeldSettingsFixture::with_the_convert_node_holding(Value::Null);
        if through_a_following_node {
            fixture
                .connect((&fixture.following, "out"), (&fixture.rgba_sink, "video"))
                .expect("a Texture of no format reaches the input with a warning");
            fixture
                .connect((&fixture.convert_node, "image"), (&fixture.following, "in"))
                .expect("rgba8_unorm reaches the rgba8_unorm input");
        } else {
            fixture
                .connect(
                    (&fixture.convert_node, "image"),
                    (&fixture.rgba_sink, "video"),
                )
                .expect("rgba8_unorm is what the input takes");
        }

        fixture.compiler.scope(|graph, _tx| {
            let differing = set_the_format_and_judge(graph, &fixture.convert_node, "bgra8_unorm");
            assert_eq!(
                differing.len(),
                1,
                "{through_a_following_node}: {differing:?}"
            );
            assert!(differing[0].contains("bgra8_unorm"), "{differing:?}");

            let differing = set_the_format_and_judge(graph, &fixture.convert_node, "rgba8_unorm");
            assert!(
                differing.is_empty(),
                "rgba8_unorm is what the input takes: {differing:?}"
            );
            assert!(
                graph
                    .traversal()
                    .e(())
                    .iter()
                    .all(|link| !link.has::<LinkPinTypesDifferComponent>()),
                "the mark is taken off once the link fits again"
            );
        });
    }
}
