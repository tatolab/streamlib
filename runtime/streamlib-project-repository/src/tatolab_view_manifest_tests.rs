// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;
use serde_json::{Value, json};

use super::*;

/// The manifests the view SDK's Zod schema is tested against, with how it judges each, recorded
/// from tatolab-app's `packages/tato-app-view-sdk/tatolab_view_contract_examples.json`; a change
/// there is recorded here again.
const VIEW_SDK_TATOLAB_VIEW_CONTRACT_EXAMPLES: &str =
    include_str!("../view-contract/tatolab_view_contract_examples.json");

#[derive(Deserialize)]
struct TatolabViewContractExamples {
    examples: Vec<TatolabViewContractExample>,
}

#[derive(Deserialize)]
struct TatolabViewContractExample {
    named: String,
    manifest: Value,
    opens: bool,
    refused_at: Option<String>,
    reads_as: Option<Value>,
}

/// `value` with every `null` field left out and every number as an `f64`, as the view SDK's
/// reading of a manifest is written.
fn as_the_view_sdk_writes_a_reading(value: Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .filter(|(_, field)| !field.is_null())
                .map(|(name, field)| (name, as_the_view_sdk_writes_a_reading(field)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(as_the_view_sdk_writes_a_reading)
                .collect(),
        ),
        Value::Number(number) => json!(number.as_f64()),
        other => other,
    }
}

#[test]
fn the_runtime_judges_every_view_contract_example_as_the_view_sdk_does() {
    let contract_examples: TatolabViewContractExamples =
        serde_json::from_str(VIEW_SDK_TATOLAB_VIEW_CONTRACT_EXAMPLES)
            .expect("the recorded examples");
    let mut disagreements = Vec::new();
    for example in contract_examples.examples {
        let named = &example.named;
        match (read_tatolab_view_manifest(&example.manifest), example.opens) {
            (Ok(manifest), true) => {
                if let Some(reads_as) = example.reads_as {
                    let read = as_the_view_sdk_writes_a_reading(
                        serde_json::to_value(&manifest).expect("a manifest serializes"),
                    );
                    let reads_as = as_the_view_sdk_writes_a_reading(reads_as);
                    if read != reads_as {
                        disagreements.push(format!(
                            "{named}: the runtime reads {read}, the view SDK {reads_as}"
                        ));
                    }
                }
            }
            (Err(because), true) => disagreements.push(format!(
                "{named}: the view SDK opens it, the runtime refuses it: {because}"
            )),
            (Ok(_), false) => disagreements.push(format!(
                "{named}: the view SDK refuses it, the runtime opens it"
            )),
            (Err(because), false) => {
                if let Some(refused_at) = example.refused_at
                    && !because.contains(&format!("`{refused_at}`:"))
                {
                    disagreements.push(format!(
                        "{named}: the view SDK refuses it at `{refused_at}`, the runtime: {because}"
                    ));
                }
            }
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}

fn a_node_view_manifest() -> Value {
    json!({
        "contract": "1.0",
        "title": "DJ Deck",
        "icon": "icon.svg",
        "entry": "index.html",
        "placement": "node",
        "inputs": [{ "name": "waveform", "family": "channels" }],
        "outputs": [{ "name": "fader", "family": "array", "range": [0, 1] }],
        "capabilities": ["camera", "camera"],
    })
}

#[test]
fn a_node_view_manifest_reads_with_its_defaults_filled_in() {
    let manifest = read_tatolab_view_manifest(&a_node_view_manifest()).expect("it reads");

    assert_eq!(manifest.title, "DJ Deck");
    assert_eq!(manifest.placement, TatolabViewPlacement::Node);
    assert_eq!(manifest.inputs[0].family, PortFamily::Channels);
    assert_eq!(manifest.outputs[0].range, Some([0.0, 1.0]));
    assert_eq!(manifest.capabilities, vec![TatolabViewCapability::Camera]);
    assert!(manifest.accepts.files.is_empty());
}

#[test]
fn a_field_the_contract_does_not_know_is_refused_naming_it() {
    let mut manifest = a_node_view_manifest();
    manifest["id"] = json!("acme-dj:deck");

    let refusal = read_tatolab_view_manifest(&manifest).expect_err("an id is never written");

    assert!(refusal.contains("unknown field `id`"), "{refusal}");
}

#[test]
fn a_sandbox_or_network_hosts_are_refused_since_every_view_runs_with_full_access() {
    for (field, value) in [
        ("sandbox", json!(true)),
        ("network", json!(["example.com"])),
    ] {
        let mut manifest = a_node_view_manifest();
        manifest[field] = value;

        let refusal =
            read_tatolab_view_manifest(&manifest).expect_err("view contract 1.0 has neither");

        assert!(
            refusal.starts_with("`tatolab_view.json`:")
                && refusal.contains(&format!("unknown field `{field}`")),
            "{refusal}"
        );
    }
}

#[test]
fn each_way_a_manifest_falls_short_is_named_by_its_field() {
    let refusal = read_tatolab_view_manifest(&json!({
        "contract": "2.0",
        "title": "  ",
        "entry": "../escape.html",
        "placement": "area",
        "outputs": [
            { "name": "Fader", "family": "records", "range": [1, 0] },
            { "name": "Fader", "family": "image" },
        ],
    }))
    .expect_err("nothing about it opens");

    for expected in [
        "`contract`: this runtime reads view contract 1.1.",
        "`title`: it is the name the view-type menu lists.",
        "`entry`: it is a path inside the view's folder",
        "`outputs[0].name`: a port name is lower-case letters, digits and underscores.",
        "`outputs[1].name`: `Fader` is named twice.",
        "`outputs[0].range`: a range is [lowest, highest], lowest first.",
        "`outputs[0].range`: only an Array port carries a range.",
        "`placement`: an `area` view has no node",
    ] {
        assert!(refusal.contains(expected), "{expected} in {refusal}");
    }
}

#[test]
fn a_settings_declaration_whose_setting_gives_no_default_does_not_open() {
    let mut manifest = a_node_view_manifest();
    manifest["settings"] = json!({
        "type": "object",
        "properties": { "gain": { "type": "number" } },
    });

    let refusal = read_tatolab_view_manifest(&manifest).expect_err("a setting needs a default");

    assert_eq!(
        refusal,
        "`settings.properties.gain.default`: every setting gives its `default`."
    );
}

#[test]
fn an_output_named_after_one_of_its_inputs_is_refused_by_name() {
    let mut manifest = a_node_view_manifest();
    manifest["outputs"] = json!([
        { "name": "fader", "family": "array" },
        { "name": "waveform", "family": "channels" }
    ]);

    let refusal = read_tatolab_view_manifest(&manifest)
        .expect_err("an output and an input would be one name on one node");

    assert!(
        refusal.starts_with("`outputs[1].name`: `waveform` names an input too"),
        "{refusal}"
    );
    assert!(!refusal.contains("outputs[0]"), "{refusal}");
}

#[test]
fn an_output_a_panel_writes_named_after_one_of_its_settings_is_refused_by_name() {
    let mut manifest = a_node_view_manifest();
    manifest["settings"] = json!({
        "type": "object",
        "properties": { "fader": { "type": "number" } }
    });

    let refusal = read_tatolab_view_manifest(&manifest)
        .expect_err("an output and a setting would be one input name on one node");

    assert!(
        refusal.starts_with("`outputs[0].name`: `fader` names a setting too"),
        "{refusal}"
    );
}

#[test]
fn an_image_or_packets_output_named_after_one_of_its_inputs_is_refused_by_name() {
    let mut manifest = a_node_view_manifest();
    manifest["inputs"] = json!([
        { "name": "preview", "family": "image" },
        { "name": "midi", "family": "packets" }
    ]);
    manifest["outputs"] = json!([
        { "name": "preview", "family": "image" },
        { "name": "midi", "family": "packets" }
    ]);

    let refusal = read_tatolab_view_manifest(&manifest)
        .expect_err("an output and an input would be one name on one node");

    assert!(
        refusal.contains("`outputs[0].name`: `preview` names an input too"),
        "{refusal}"
    );
    assert!(
        refusal.contains("`outputs[1].name`: `midi` names an input too"),
        "{refusal}"
    );
}

#[test]
fn an_image_or_packets_output_may_share_its_name_with_a_setting() {
    let mut manifest = a_node_view_manifest();
    manifest["outputs"] = json!([
        { "name": "preview", "family": "image" },
        { "name": "midi", "family": "packets" }
    ]);
    manifest["settings"] = json!({
        "type": "object",
        "properties": {
            "preview": { "type": "boolean", "default": true },
            "midi": { "type": "number", "default": 1 }
        }
    });

    let manifest = read_tatolab_view_manifest(&manifest)
        .expect("no input of an Image or Packets output's name is made, so a setting takes it");

    assert_eq!(manifest.outputs.len(), 2);
}

fn a_node_view_declaring(contract: &str, outputs: Value) -> Value {
    json!({
        "contract": contract,
        "title": "Typed",
        "entry": "index.html",
        "placement": "node",
        "outputs": outputs,
    })
}

#[test]
fn a_port_naming_its_pin_type_serves_the_family_its_messages_belong_to() {
    let manifest = read_tatolab_view_manifest(&a_node_view_declaring(
        "1.1",
        json!([
            { "name": "picture", "pin_type": { "type": "texture" } },
            { "name": "level", "family": "channels", "pin_type": { "type": "float" } },
            { "name": "fader", "family": "array" },
        ]),
    ))
    .expect("a 1.1 view opens");
    let read: Vec<_> = manifest
        .outputs
        .iter()
        .map(|port| (port.name.as_str(), port.family, port.pin_type.clone()))
        .collect();
    assert_eq!(
        read,
        [
            (
                "picture",
                PortFamily::Image,
                Some(PinType::Texture {
                    pixel_formats: vec![]
                })
            ),
            (
                "level",
                PortFamily::Channels,
                Some(PinType::Float { exact_size: None })
            ),
            ("fader", PortFamily::Array, None),
        ]
    );
}

#[test]
fn a_port_with_no_pin_type_or_family_or_one_that_disagrees_is_refused_at_the_port() {
    for (outputs, expected) in [
        (
            json!([{ "name": "level" }]),
            "`outputs[0]`: a port names the `pin_type` it carries, or the older `family`.",
        ),
        (
            json!([{ "name": "level", "family": "records", "pin_type": { "type": "float" } }]),
            "`outputs[0]`: `family` is `records`, and a Float is carried as `channels`; drop \
             `family`, which the `pin_type` sets.",
        ),
        (
            json!([{ "name": "items", "pin_type": { "type": "list_of", "element_type": { "type": "float" } } }]),
            "`outputs[0]`: `pin_type`: a List of Float rides no message a view's port reads or writes",
        ),
        (
            json!([{ "name": "choice", "pin_type": { "type": "menu", "option_names": [] } }]),
            "`outputs[0]`: `pin_type`: a Menu lists at least one option.",
        ),
    ] {
        let refusal = read_tatolab_view_manifest(&a_node_view_declaring("1.1", outputs))
            .expect_err("the port is refused");
        assert!(refusal.contains(expected), "{expected} in {refusal}");
    }
}

#[test]
fn an_unknown_pin_type_is_refused_by_name() {
    let refusal = read_tatolab_view_manifest(&a_node_view_declaring(
        "1.1",
        json!([{ "name": "level", "pin_type": { "type": "decibels" } }]),
    ))
    .expect_err("no such pin type");
    assert!(
        refusal.starts_with("`outputs[0].pin_type.type`:") && refusal.contains("`decibels`"),
        "{refusal}"
    );
}

#[test]
fn a_pin_type_in_a_view_built_for_contract_one_point_zero_is_refused_naming_the_minor_to_write() {
    let refusal = read_tatolab_view_manifest(&a_node_view_declaring(
        "1.0",
        json!([{ "name": "level", "pin_type": { "type": "float" } }]),
    ))
    .expect_err("1.0 has no pin_type");
    assert!(
        refusal.contains(
            "`outputs[0].pin_type`: a port's `pin_type` is view contract 1.1; write \
             `\"contract\": \"1.1\"`."
        ),
        "{refusal}"
    );
}

#[test]
fn an_output_a_panel_writes_of_a_type_that_holds_no_value_is_refused() {
    let refusal = read_tatolab_view_manifest(&a_node_view_declaring(
        "1.1",
        json!([{ "name": "go", "pin_type": { "type": "trigger" } }]),
    ))
    .expect_err("a panel writes no Trigger value");
    assert!(
        refusal.contains(
            "`outputs[0].pin_type`: a panel writes a value onto a `records` output, and a \
             Trigger holds none."
        ),
        "{refusal}"
    );
    read_tatolab_view_manifest(&a_node_view_declaring(
        "1.1",
        json!([{ "name": "voice", "pin_type": { "type": "audio" } },
               { "name": "weights", "pin_type": { "type": "tensor" } }]),
    ))
    .expect("a panel writes Audio samples and Tensor numbers");
}
