// Copyright (c) 2025 Jonathan Fontanez
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
        "`contract`: this runtime reads view contract 1.0.",
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
fn a_path_inside_the_views_folder_has_no_scheme_root_or_parent_step() {
    for inside in ["index.html", "assets/icon.svg", "a..b/c"] {
        assert!(is_a_path_inside_the_views_folder(inside), "{inside}");
    }
    for outside in [
        "",
        "/index.html",
        "../index.html",
        "a/./b",
        "a//b",
        "a\\b",
        "https://example.com/x",
        "data:text/html,x",
    ] {
        assert!(!is_a_path_inside_the_views_folder(outside), "{outside}");
    }
}
