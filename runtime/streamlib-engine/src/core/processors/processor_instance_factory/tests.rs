// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName};

fn class_import_path(path: &str) -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(path).expect("the fixture path names a class")
}

/// The short name is held constant while the import path varies. A test
/// that varied both together could not tell which one the registry
/// actually keyed on.
fn descriptor_for(path: &str) -> ProcessorDescriptor {
    ProcessorDescriptor::new(
        ProcessorClassShortName::new("HeldConstant").unwrap(),
        class_import_path(path),
        "test",
    )
}

#[test]
fn a_processor_is_registered_and_looked_up_by_its_class_import_path() {
    let factory = ProcessorInstanceFactory::new();
    let blur = class_import_path("my_app.filters:BlurProcessor");

    assert!(
        factory.descriptor(&blur).is_none(),
        "nothing is registered until it is"
    );

    factory
        .register_descriptor_only(descriptor_for(blur.as_str()))
        .expect("registering a fresh import path succeeds");

    assert!(factory.descriptor(&blur).is_some());
    assert!(factory.port_info(&blur).is_some());
    assert_eq!(
        factory
            .descriptor(&blur)
            .unwrap()
            .processor_class_import_path,
        blur
    );
}

/// The two grammars share one registry and never collide with each other:
/// the key is the whole string, so nothing canonicalizes a Python path into
/// a Rust one or back.
#[test]
fn distinct_import_paths_do_not_collide() {
    let factory = ProcessorInstanceFactory::new();
    let paths = [
        "my_app.filters:BlurProcessor",
        "my_app::filters::BlurProcessor",
        "other_app.filters:BlurProcessor",
        "my_app.effects:BlurProcessor",
    ];

    for path in paths {
        factory
            .register_descriptor_only(descriptor_for(path))
            .unwrap_or_else(|e| panic!("{path} must register cleanly: {e}"));
    }

    assert_eq!(factory.list_registered().len(), paths.len());
    for path in paths {
        assert!(
            factory.descriptor(&class_import_path(path)).is_some(),
            "{path} must stay addressable by its own path"
        );
    }
}

/// The registration that arrived first stays, and the refusal names both
/// languages' causes — a Rust processor declared in a function body derives
/// its enclosing module's path, so two of them collide here and nowhere
/// else.
#[test]
fn a_second_class_claiming_a_registered_import_path_is_refused() {
    let factory = ProcessorInstanceFactory::new();
    let path = "my_app.filters:BlurProcessor";

    factory
        .register_descriptor_only(descriptor_for(path))
        .expect("first registration succeeds");

    let refusal = factory
        .register_descriptor_only(descriptor_for(path))
        .expect_err("a duplicate import path must be refused");

    let Error::Configuration(message) = refusal else {
        panic!("expected Configuration; got {refusal:?}");
    };
    assert!(
        message.contains(path),
        "the refusal must name the contested path; got: {message}"
    );
    assert!(
        message.contains("importlib.reload"),
        "the refusal must name the Python cause; got: {message}"
    );
    assert!(
        message.contains("module scope"),
        "the refusal must name the Rust cause and its fix; got: {message}"
    );
    assert_eq!(
        factory.list_registered().len(),
        1,
        "a refused duplicate must not overwrite the live registration"
    );
}

/// Two `#[processor]` types declared inside function bodies in one module
/// derive the same `module_path!()`. Under the structured key their
/// distinct idents hid the clash and neither was reachable by `use`; the
/// import-path key surfaces it at registration.
#[test]
fn two_rust_processors_sharing_a_module_path_are_caught_at_registration() {
    let factory = ProcessorInstanceFactory::new();
    let shared_module_path = "my_crate::filters";

    factory
        .register_descriptor_only(descriptor_for(shared_module_path))
        .expect("the first of the two registers");

    assert!(
        factory
            .register_descriptor_only(descriptor_for(shared_module_path))
            .is_err(),
        "the second must be refused rather than silently shadowing the first"
    );
}

/// Two threads racing to claim one path: exactly one wins, and the loser
/// is refused rather than overwriting. Narrow the `descriptors` guard back
/// to a `read()` that ends before the inserts and both threads pass the
/// check, so both insert and the second silently displaces the first.
#[test]
fn concurrent_registrations_of_one_path_leave_exactly_one_winner() {
    use std::sync::{Arc, Barrier};

    // Repeated, because losing this race once is a scheduling accident —
    // a single round would pass against the broken code most of the time.
    for _ in 0..200 {
        let factory = Arc::new(ProcessorInstanceFactory::new());
        let both_threads_ready = Arc::new(Barrier::new(2));

        let contenders: Vec<_> = (0..2)
            .map(|_| {
                let factory = Arc::clone(&factory);
                let both_threads_ready = Arc::clone(&both_threads_ready);
                std::thread::spawn(move || {
                    both_threads_ready.wait();
                    factory
                        .register_descriptor_only(descriptor_for("my_app.filters:RacedProcessor"))
                        .is_ok()
                })
            })
            .collect();

        let winners = contenders
            .into_iter()
            .map(|contender| contender.join().expect("no contender may panic"))
            .filter(|registered| *registered)
            .count();

        assert_eq!(winners, 1, "exactly one registration may claim a path");
        assert_eq!(factory.list_registered().len(), 1);
    }
}

#[test]
fn an_unregistered_path_is_absent_from_every_map() {
    let factory = ProcessorInstanceFactory::new();
    factory
        .register_descriptor_only(descriptor_for("my_app.filters:BlurProcessor"))
        .unwrap();

    let never_registered = class_import_path("my_app.filters:SharpenProcessor");
    assert!(factory.descriptor(&never_registered).is_none());
    assert!(factory.port_info(&never_registered).is_none());
    assert!(!factory.is_registered(&never_registered));
    assert!(!factory.can_create(&never_registered));
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
    }
}

/// Both registration paths refuse one, and nothing of the refused
/// descriptor stays registered.
fn assert_refused_by_either_registration(
    descriptor: ProcessorDescriptor,
    is_the_expected_refusal: impl Fn(&Error) -> bool,
) {
    let path = descriptor.processor_class_import_path.clone();
    let factory = ProcessorInstanceFactory::new();

    let refusal = factory
        .register_descriptor_only(descriptor.clone())
        .expect_err("register_descriptor_only must refuse it");
    assert!(is_the_expected_refusal(&refusal), "got {refusal:?}");

    let constructor: DynamicProcessorConstructorFn =
        Box::new(|_node| Err(Error::Configuration("unreachable".into())));
    let node_types_one_stream_resolves = crate::core::processors::NodeTypesOneStreamResolves::new();
    let refusal = node_types_one_stream_resolves
        .register_a_type_described_in_this_streams_processor_interpreter(
            descriptor,
            crate::core::processors::HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
                constructor,
            ),
        )
        .expect_err("a describe's registration must refuse it");
    assert!(is_the_expected_refusal(&refusal), "got {refusal:?}");

    assert!(factory.descriptor(&path).is_none());
    assert!(factory.port_info(&path).is_none());
    assert!(node_types_one_stream_resolves.descriptor(&path).is_none());
    assert!(node_types_one_stream_resolves.port_info(&path).is_none());
}

#[test]
fn a_descriptor_whose_port_name_is_not_cast_is_refused_by_name() {
    let path = "my_app.filters:BlurProcessor";
    assert_refused_by_either_registration(
        descriptor_for(path).with_output(port_named("videoOut")),
        |refusal| {
            matches!(
                refusal,
                Error::DescriptorPortNameNotCast { processor_class_import_path, port_name }
                    if processor_class_import_path.as_str() == path && port_name == "videoOut"
            )
        },
    );
}

#[test]
fn a_descriptor_declaring_one_port_name_twice_is_refused_by_name() {
    let path = "my_app.filters:BlurProcessor";
    assert_refused_by_either_registration(
        descriptor_for(path)
            .with_input(port_named("video"))
            .with_output(port_named("video")),
        |refusal| {
            matches!(
                refusal,
                Error::DescriptorPortNameDeclaredTwice { processor_class_import_path, port_name }
                    if processor_class_import_path.as_str() == path && port_name == "video"
            )
        },
    );
}

/// The port marker names the port in the author's spelling and resolves to
/// the cast the descriptor registered.
#[test]
fn a_rust_processor_declaring_a_camel_case_port_registers_its_cast() {
    use crate::core::graph::OutputPortMarker;
    use crate::core::test_support::MockProcessorWithACamelCaseOutputPort;

    let factory = ProcessorInstanceFactory::new();
    factory.register::<MockProcessorWithACamelCaseOutputPort::Processor>();

    let descriptor = factory
        .descriptor(&MockProcessorWithACamelCaseOutputPort::processor_class_import_path())
        .expect("the macro-declared processor registers");
    let output_port_names: Vec<&str> = descriptor
        .outputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(output_port_names, ["outone"]);
    assert_eq!(
        <MockProcessorWithACamelCaseOutputPort::OutputLink::outOne as OutputPortMarker>::PORT_NAME,
        "outone"
    );
}

#[test]
fn a_typed_registration_refuses_a_config_naming_the_setting_and_a_python_class_takes_any() {
    use crate::core::test_support::MockSourceTakingOneSetting;
    let factory = ProcessorInstanceFactory::new();
    factory.register::<MockSourceTakingOneSetting::Processor>();
    let python_class = class_import_path("my_app.nodes:Blur");
    factory
        .register_descriptor_only(descriptor_for(python_class.as_str()))
        .unwrap();
    let typed = MockSourceTakingOneSetting::processor_class_import_path();
    let a_misspelled_setting = serde_json::json!({"frame_widht": 640});

    let refusal = factory
        .refuse_a_config_the_node_type_does_not_take("front", &typed, &a_misspelled_setting)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("node `front`"), "{refusal}");
    assert!(refusal.contains(typed.as_str()), "{refusal}");
    assert!(refusal.contains("`frame_widht`"), "{refusal}");
    let a_setting_of_the_wrong_kind = factory
        .refuse_a_config_the_node_type_does_not_take(
            "front",
            &typed,
            &serde_json::json!({"frame_width": "wide"}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        a_setting_of_the_wrong_kind.contains("frame_width: invalid type"),
        "{a_setting_of_the_wrong_kind}"
    );
    factory
        .refuse_a_config_the_node_type_does_not_take(
            "front",
            &typed,
            &serde_json::json!({"frame_width": 640}),
        )
        .unwrap();
    factory
        .refuse_a_config_the_node_type_does_not_take("blur", &python_class, &a_misspelled_setting)
        .unwrap();
}

#[test]
fn a_built_in_that_moved_to_a_module_is_refused_naming_the_module_to_install() {
    let factory = ProcessorInstanceFactory::new();
    let moved_out = class_import_path("tatolab.stream:MovedOut");
    let module_node_type = class_import_path("tatolab.moved:MovedOut");
    factory.register_built_in_node_type_moved_to_a_module(
        moved_out.clone(),
        module_node_type.clone(),
        "tatolab-moved",
    );

    let refusal = factory
        .refuse_a_node_this_runtime_cannot_add("moved", &moved_out, &serde_json::json!({}))
        .unwrap_err();

    assert!(
        matches!(
            &refusal,
            Error::BuiltInNodeTypeMovedToAModule {
                node_type,
                module_node_type: refused_naming,
                module_distribution,
            } if *node_type == moved_out
                && *refused_naming == module_node_type
                && module_distribution == "tatolab-moved"
        ),
        "{refusal:?}"
    );
    let message = refusal.to_string();
    assert!(message.contains("`tatolab.moved:MovedOut`"), "{message}");
    assert!(message.contains("tatolab-moved"), "{message}");
}
