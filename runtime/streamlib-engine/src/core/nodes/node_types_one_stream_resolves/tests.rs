// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::{NodeClassShortName, PortDescriptor};

fn class_import_path(path: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(path).expect("the fixture path names a class")
}

fn descriptor_for(path: &str) -> NodeDescriptor {
    NodeDescriptor::new(
        NodeClassShortName::new("HeldConstant").unwrap(),
        class_import_path(path),
        "test",
    )
}

fn port_named(name: &str) -> PortDescriptor {
    PortDescriptor {
        name: name.to_string(),
        description: String::new(),
        required: true,
        delivery_profile: None,
        borrow_quota: None,
        audio_window: None,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
        pin_default: None,
        pin_declaration: None,
        lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
        input_mode: None,
    }
}

/// A constructor that refuses with `refusal`, so a test can tell whose
/// constructor a create reached.
fn a_constructor_refusing_with(refusal: &'static str) -> DynamicNodeConstructorFn {
    Box::new(move |_node| Err(Error::NotSupported(refusal.to_string())))
}

fn a_node_of(node_type: &NodeClassImportPath) -> GraphNode {
    GraphNode::new(node_type.clone(), None, "a-node", None, vec![], vec![])
}

/// A described type stating a Texture-to-Texture conversion, at `path`.
fn a_described_conversion(path: &str) -> NodeDescriptor {
    use crate::core::descriptors::{PinDeclaration, PinType};
    let texture = PinDeclaration::exactly(PinType::Texture {
        pixel_formats: vec![],
    });
    let typed = |name| port_named(name).with_pin_declaration(texture.clone());
    descriptor_for(path)
        .with_input(typed("video"))
        .with_output(typed("out"))
        .stating_a_conversion()
}

fn an_installed_module() -> InstalledModuleOfANodeType {
    InstalledModuleOfANodeType {
        distribution: "acme-filters".to_owned(),
        distribution_version: "1.0.0".to_owned(),
        entry_point: "filters".to_owned(),
    }
}

fn the_conversions_of(node_types: &NodeTypesOneStreamResolves) -> Vec<NodeClassImportPath> {
    node_types
        .pin_conversions()
        .iter()
        .map(|conversion| conversion.node_type.clone())
        .collect()
}

/// A conversion an installed module declares is registered and offered to the verdict, and
/// kept when a load of a graph naming the type describes it again.
#[test]
fn a_conversion_an_installed_module_declares_is_registered_and_offered() {
    let node_types = NodeTypesOneStreamResolves::new();
    let path = "node_types_one_stream_resolves_tests.installed:ConvertsATexture";
    node_types
        .register_a_type_an_installed_module_declares(
            a_described_conversion(path),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "an installed module's conversion",
            )),
            an_installed_module(),
        )
        .expect("an installed module may state a conversion");
    assert!(the_conversions_of(&node_types).contains(&class_import_path(path)));
    assert_eq!(
        node_types.node_types_installed_modules_declare().len(),
        1,
        "the module declaring it is recorded"
    );

    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_conversion(path),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "described again by a load",
            )),
        )
        .expect("a type an installed module declares keeps its conversion");
    assert!(the_conversions_of(&node_types).contains(&class_import_path(path)));
}

/// A conversion a project's own node type states is registered and offered to the verdict, as
/// an installed module's is.
///
/// Mental-revert: refusing a conversion no installed module is recorded as declaring refuses
/// the project's node type, and the verdict never names it.
#[test]
fn a_conversion_a_projects_node_type_states_is_registered_and_offered() {
    let node_types = NodeTypesOneStreamResolves::new();
    let path = "node_types_one_stream_resolves_tests.project:ConvertsATexture";
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_conversion(path),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "a project's conversion",
            )),
        )
        .expect("a project's node type may state a conversion");
    assert!(the_conversions_of(&node_types).contains(&class_import_path(path)));
    assert!(node_types.was_described_in_this_streams_node_interpreter(&class_import_path(path)));
}

/// A described conversion whose name a view of the project also has is never offered: the
/// stream resolves that name as the view, which converts nothing.
///
/// Mental-revert: listing every described type's conversion offers the verdict one the view the
/// stream resolves does not perform.
#[test]
fn a_conversion_a_view_of_the_same_name_shadows_is_never_offered() {
    let node_types = NodeTypesOneStreamResolves::new();
    let path = "a-project-bringing-a-view:program";
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_conversion(path),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "a conversion the view shadows",
            )),
        )
        .expect("the described type registers");
    assert!(the_conversions_of(&node_types).contains(&class_import_path(path)));

    node_types.replace_the_view_types_this_streams_project_brings(&[
        crate::core::test_support::a_view_of_a_project_reading_records(path),
    ]);

    assert!(node_types.is_a_view_type_of_this_streams_project(&class_import_path(path)));
    assert!(!the_conversions_of(&node_types).contains(&class_import_path(path)));
}

/// A later describe of a type into one stream replaces what the earlier
/// one registered — the class's ports may have changed since.
#[test]
fn a_later_describe_into_a_stream_replaces_the_earlier_ones_registration() {
    let node_types = NodeTypesOneStreamResolves::new();
    let path = "node_types_one_stream_resolves_tests.filters:ReDescribedNode";

    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            descriptor_for(path).with_input(port_named("video")),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "first describe",
            )),
        )
        .expect("a first describe registers the type");
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            descriptor_for(path).with_input(port_named("frames")),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "later describe",
            )),
        )
        .expect("a later describe replaces it");

    let GraphNodePorts { inputs, .. } = node_types
        .port_info(&class_import_path(path))
        .expect("the type is registered");
    let input_names: Vec<_> = inputs.iter().map(|port| port.name.as_str()).collect();
    assert_eq!(input_names, ["frames"]);
    assert!(node_types.was_described_in_this_streams_node_interpreter(&class_import_path(path)));
    assert!(!node_types.is_a_natively_compiled_node_type(&class_import_path(path)));
}

/// A typed Rust registration and a descriptor alone are never replaced by
/// a describe claiming their path: a Python class cannot quietly displace
/// a native built-in.
#[test]
fn a_describe_never_replaces_a_natively_compiled_node_type() {
    use crate::core::test_support::{MockSourceTakingOneSetting, ensure_test_mocks_registered};
    ensure_test_mocks_registered();
    let typed = MockSourceTakingOneSetting::node_class_import_path();
    let descriptor_only =
        class_import_path("node_types_one_stream_resolves_tests.filters:RegisteredAsADescriptor");
    if NODE_REGISTRY.descriptor(&descriptor_only).is_none() {
        NODE_REGISTRY
            .register_descriptor_only(descriptor_for(descriptor_only.as_str()))
            .expect("the descriptor registers");
    }
    let node_types = NodeTypesOneStreamResolves::new();

    for registered_natively in [&typed, &descriptor_only] {
        assert!(node_types.is_a_natively_compiled_node_type(registered_natively));
        let refused = node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor_for(registered_natively.as_str()),
                HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                    a_constructor_refusing_with("a describe that must be refused"),
                ),
            )
            .expect_err("a describe never replaces a natively compiled type");
        assert!(
            refused.to_string().contains(registered_natively.as_str()),
            "{refused}"
        );
        assert!(!node_types.was_described_in_this_streams_node_interpreter(registered_natively));
    }
}

/// Two streams describing one class each resolve their own descriptor,
/// ports and constructor: neither starts its nodes from the other's
/// interpreter, and neither registration reaches the machine's registry.
#[test]
fn two_streams_describing_one_class_each_resolve_their_own() {
    let first_stream = NodeTypesOneStreamResolves::new();
    let second_stream = NodeTypesOneStreamResolves::new();
    let path = class_import_path("node_types_one_stream_resolves_tests.filters:SharedClass");

    first_stream
        .register_a_type_described_in_this_streams_node_interpreter(
            descriptor_for(path.as_str()).with_input(port_named("video")),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "the first stream's interpreter",
            )),
        )
        .expect("the first stream describes the class");
    second_stream
        .register_a_type_described_in_this_streams_node_interpreter(
            descriptor_for(path.as_str()).with_input(port_named("frames")),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "the second stream's interpreter",
            )),
        )
        .expect("the second stream describes the class");

    let input_names_of = |node_types: &NodeTypesOneStreamResolves| -> Vec<String> {
        node_types
            .port_info(&path)
            .expect("the stream resolves the class")
            .inputs
            .into_iter()
            .map(|port| port.name)
            .collect()
    };
    assert_eq!(input_names_of(&first_stream), ["video"]);
    assert_eq!(input_names_of(&second_stream), ["frames"]);

    let created_by = |node_types: &NodeTypesOneStreamResolves| -> String {
        match node_types.create(&a_node_of(&path)) {
            Ok(_) => panic!("the test constructor never builds a node"),
            Err(refusal) => refusal.to_string(),
        }
    };
    assert!(created_by(&first_stream).contains("the first stream's interpreter"));
    assert!(created_by(&second_stream).contains("the second stream's interpreter"));

    assert!(NODE_REGISTRY.descriptor(&path).is_none());
    assert!(
        NodeTypesOneStreamResolves::new()
            .descriptor(&path)
            .is_none(),
        "a stream that described nothing resolved another stream's class"
    );
    assert!(
        first_stream
            .node_catalog()
            .iter()
            .any(|descriptor| descriptor.node_class_import_path == path),
        "the stream's node catalog leaves out the class it described"
    );
}

/// The conversion `path` states as `node_types` offers it.
fn the_conversion_of(node_types: &NodeTypesOneStreamResolves, path: &str) -> PinConversion {
    node_types
        .pin_conversions()
        .iter()
        .find(|conversion| conversion.node_type == class_import_path(path))
        .cloned()
        .unwrap_or_else(|| panic!("`{path}` states a conversion"))
}

/// A function node's conversion casts a saved value with the cast the stream gives each, which
/// names it and its input, and a node class's converts only inside a started stream.
///
/// Mental-revert: giving every described conversion the cast runs a node class's body out of
/// stream; giving none leaves the function node's saved value to the default.
#[test]
fn a_function_node_conversion_casts_a_saved_value_and_a_node_class_conversion_does_not() {
    let node_types = NodeTypesOneStreamResolves::new();
    node_types.cast_a_saved_value_through_each_function_node_conversion_by(|conversion| {
        let named = format!("{} on {}", conversion.node_type, conversion.input_port_name);
        PinConversionSavedValueCast::new(move |value, _into| {
            Ok(serde_json::json!(format!("{value} by {named}")))
        })
    });
    let function_node = "node_types_one_stream_resolves_tests.project:ConvertsByItsBody";
    let node_class = "node_types_one_stream_resolves_tests.project:ConvertsInAStream";
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_conversion(function_node),
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: a_constructor_refusing_with(
                    "a function node conversion",
                ),
            },
        )
        .expect("a function node may state a conversion");
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_conversion(node_class),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(a_constructor_refusing_with(
                "a node class conversion",
            )),
        )
        .expect("a node class may state a conversion");

    let cast = the_conversion_of(&node_types, function_node)
        .saved_value_cast
        .expect("a function node conversion casts a saved value");
    assert_eq!(
        cast.cast(
            &serde_json::json!(1),
            &crate::core::descriptors::PinType::String {}
        ),
        Ok(serde_json::json!(format!("1 by {function_node} on video")))
    );
    assert_eq!(
        the_conversion_of(&node_types, node_class).saved_value_cast,
        None
    );
}
