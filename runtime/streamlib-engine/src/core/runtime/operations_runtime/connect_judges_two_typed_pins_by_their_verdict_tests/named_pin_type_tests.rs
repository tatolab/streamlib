// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A named type connects only to the same name, and only to a typed pin: its messages are the
//! bytes its class packs, which belong to no family, so a link between it and an untyped port is
//! refused naming the type and both ends, whatever family that port reads.

use super::*;
use crate::core::descriptors::DeclaredPinType;

const UNTYPED_RECORDS_SOURCE: &str = "VerdictCheckUntypedRecordsSource";
const UNTYPED_RECORDS_SINK: &str = "VerdictCheckUntypedRecordsSink";
const ANY_RECORDS_SINK: &str = "VerdictCheckAnyRecordsSink";
const ANY_RECORDS_RELAY: &str = "VerdictCheckAnyRecordsRelay";

fn an_untyped_records_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(name, name, true, PinDeclaration::exactly(PinType::String {}))
}

fn register_the_named_type_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    register_the_verdict_fixture_node_types();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register(a_node_type(
            class_path(UNTYPED_RECORDS_SOURCE),
            Vec::new(),
            vec![an_untyped_records_port("records")],
        ));
        register(a_node_type(
            class_path(UNTYPED_RECORDS_SINK),
            vec![an_untyped_records_port("records")],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(ANY_RECORDS_SINK),
            vec![
                an_untyped_records_port("anything").with_pin_declaration(PinDeclaration {
                    pin_type: DeclaredPinType::Any,
                    ..PinDeclaration::exactly(PinType::String {})
                }),
            ],
            Vec::new(),
        ));
        register(a_node_type(
            class_path(ANY_RECORDS_RELAY),
            vec![
                an_untyped_records_port("in").with_pin_declaration(PinDeclaration {
                    pin_type: DeclaredPinType::Any,
                    ..PinDeclaration::exactly(PinType::String {})
                }),
            ],
            vec![
                an_untyped_records_port("out").with_pin_declaration(PinDeclaration {
                    pin_type: DeclaredPinType::SameAsInput {
                        input_port_name: "in".to_owned(),
                    },
                    ..PinDeclaration::exactly(PinType::String {})
                }),
            ],
        ));
    });
}

#[test]
fn two_pins_of_one_named_type_connect() {
    register_the_named_type_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(KELVIN_SOURCE));
    let sink = a_node_of(&compiler, class_path(KELVIN_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "temperature"),
        InputLinkPortRef::new(sink, "temperature"),
    )
    .expect("two pins typed `verdict_check:Kelvin` connect");
    assert_eq!(the_links_in(&compiler), 1);
}

/// Mental-revert: judging two named types by their families alone takes the link, as both ports
/// are Records.
#[test]
fn a_named_type_into_another_name_is_refused_naming_both_types() {
    register_the_named_type_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(CELSIUS_SOURCE));
    let sink = a_node_of(&compiler, class_path(KELVIN_SINK));

    let refusal = the_verdicts_refusal(
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "temperature"),
            InputLinkPortRef::new(sink, "temperature"),
        )
        .expect_err("two named types of other paths do not connect"),
    );

    let words = refusal.to_string();
    assert!(words.contains("`verdict_check:Celsius`"), "{words}");
    assert!(words.contains("`verdict_check:Kelvin`"), "{words}");
    assert_eq!(the_links_in(&compiler), 0);
}

/// Mental-revert: without the named type's own check, the untyped Records input takes the link
/// by its family, and would read the class's bytes as a Records message.
#[test]
fn a_named_type_into_an_untyped_input_is_refused_naming_the_type_and_both_ends() {
    register_the_named_type_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(KELVIN_SOURCE));
    let sink = a_node_of(&compiler, class_path(UNTYPED_RECORDS_SINK));

    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "temperature"),
        InputLinkPortRef::new(sink, "records"),
    )
    .expect_err("only a typed pin reads a named type");

    let words = refused.to_string();
    assert!(words.contains("`verdict_check:Kelvin`"), "{words}");
    assert!(
        words.contains(&format!("`{}.temperature`", KELVIN_SOURCE.to_lowercase())),
        "{words}"
    );
    assert!(
        words.contains(&format!(
            "`{}.records` states no pin type",
            UNTYPED_RECORDS_SINK.to_lowercase()
        )),
        "{words}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn an_untyped_output_into_a_named_type_is_refused_naming_the_type_and_both_ends() {
    register_the_named_type_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(UNTYPED_RECORDS_SOURCE));
    let sink = a_node_of(&compiler, class_path(KELVIN_SINK));

    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "records"),
        InputLinkPortRef::new(sink, "temperature"),
    )
    .expect_err("only a typed pin writes a named type");

    let words = refused.to_string();
    assert!(words.contains("`verdict_check:Kelvin`"), "{words}");
    assert!(
        words.contains(&format!(
            "`{}.records` states no pin type",
            UNTYPED_RECORDS_SOURCE.to_lowercase()
        )),
        "{words}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn an_input_of_any_type_takes_a_named_type() {
    register_the_named_type_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(KELVIN_SOURCE));
    let sink = a_node_of(&compiler, class_path(ANY_RECORDS_SINK));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "temperature"),
        InputLinkPortRef::new(sink, "anything"),
    )
    .expect("an input of any type takes every typed output, a named type's among them");
    assert_eq!(the_links_in(&compiler), 1);
}

/// An output following an input of any type carries what is linked into that input, so a named
/// type linked in would reach the untyped input its wired link feeds; the link is refused
/// naming the type and that wired link.
///
/// Mental-revert: checking only the two ends of the link made takes it, as an Any input takes a
/// named type.
#[test]
fn a_named_type_carried_past_a_relay_into_an_untyped_input_is_refused_naming_the_wired_link() {
    register_the_named_type_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, class_path(KELVIN_SOURCE));
    let relay = a_node_of(&compiler, class_path(ANY_RECORDS_RELAY));
    let sink = a_node_of(&compiler, class_path(UNTYPED_RECORDS_SINK));
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(relay.clone(), "out"),
        InputLinkPortRef::new(sink, "records"),
    )
    .expect("nothing is linked into the relay yet");

    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "temperature"),
        InputLinkPortRef::new(relay, "in"),
    )
    .expect_err("the named type would reach the untyped input");

    let words = refused.to_string();
    assert!(words.contains("`verdict_check:Kelvin`"), "{words}");
    assert!(words.contains(".records"), "{words}");
    assert!(words.contains("states no pin type"), "{words}");
    assert_eq!(the_links_in(&compiler), 1);
}
