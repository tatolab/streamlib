// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node of a conversion whose output follows a setting carries the type it gives under the
//! settings it holds into every judgement of a link: one past a node whose output follows its
//! input, a wired link past the input it feeds, and a setting changed while it is wired.

use serde_json::{Map, json};

use super::*;
use crate::core::descriptors::{
    DeclaredPinType, PinConversionOutputPick, PinConversionOutputPicked,
};
use crate::core::graph::{
    refuse_a_config_update_a_wired_link_out_of_a_convert_node_would_refuse,
    refuse_a_type_choice_a_wired_link_of_the_node_does_not_fit,
};

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
                .stating_a_conversion(),
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

/// A config update of the convert node's `format` is refused, naming the link, when a wired
/// link out of it, or past the input that link feeds, does not take the format it would then
/// write; one that keeps a format each link takes is taken.
///
/// Mental-revert: checking a config update only by the families of the links out of the node
/// takes a `format` its wired link refuses, since its output states one Texture whatever it
/// holds.
#[test]
fn a_config_update_choosing_a_format_a_wired_link_refuses_is_refused() {
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
            let refused = refuse_a_config_update_a_wired_link_out_of_a_convert_node_would_refuse(
                graph,
                &fixture.convert_node,
                &json!({ "format": "bgra8_unorm" }),
            )
            .expect_err("the wired input takes only rgba8_unorm");
            let words = refused.to_string();
            assert!(words.contains("bgra8_unorm"), "{words}");
            assert!(words.contains("config update for"), "{words}");
            refuse_a_config_update_a_wired_link_out_of_a_convert_node_would_refuse(
                graph,
                &fixture.convert_node,
                &json!({ "format": "rgba8_unorm" }),
            )
            .expect("rgba8_unorm is what the input takes");
        });
    }
}

/// A `format` set on the convert node as an input value is refused, as a config update is,
/// when its wired link does not take the format it would then write.
///
/// Mental-revert: checking only the outputs a setting types by their declaration takes the
/// value, since the convert node's output declares one Texture.
#[test]
fn a_format_set_as_an_input_value_a_wired_link_refuses_is_refused() {
    let fixture = HeldSettingsFixture::with_the_convert_node_holding(Value::Null);
    fixture
        .connect(
            (&fixture.convert_node, "image"),
            (&fixture.rgba_sink, "video"),
        )
        .expect("rgba8_unorm is what the input takes");

    fixture.compiler.scope(|graph, _tx| {
        let set_format = |graph: &mut crate::core::graph::Graph, format: &str| {
            graph
                .traversal_mut()
                .v(&fixture.convert_node)
                .first_mut()
                .expect("the node is held")
                .pins
                .insert("format".to_owned(), json!(format));
        };
        set_format(graph, "bgra8_unorm");
        refuse_a_type_choice_a_wired_link_of_the_node_does_not_fit(
            graph,
            &fixture.convert_node,
            "format",
        )
        .expect_err("the wired input takes only rgba8_unorm");
        set_format(graph, "rgba8_unorm");
        refuse_a_type_choice_a_wired_link_of_the_node_does_not_fit(
            graph,
            &fixture.convert_node,
            "format",
        )
        .expect("rgba8_unorm is what the input takes");
    });
}
