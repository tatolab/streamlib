// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An agent reads one vocabulary: pins, pin types and input values. Everything it is handed is
//! read here for the words of the forms leaving the contract, and the only text excused is what
//! the contract itself marks as leaving: an operation that states `x-replaced-by`, and a schema
//! that states `x-removed-in`.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::{LOCAL_API_MCP_SERVER_INSTRUCTIONS, control_contract_document};

/// A family's name as a declaration spells it. Only the capitalised spelling is a family: in
/// lower case each is an ordinary word ("log records", "an array of numbers").
const FAMILY_NAMES: [&str; 5] = ["Image", "Array", "Channels", "Packets", "Records"];

/// The leaving word `text` says, if it says one.
fn the_leaving_word_in(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for word in ["config", "famil"] {
        if lower.contains(word) {
            return Some(word.to_owned());
        }
    }
    text.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|token| FAMILY_NAMES.contains(token))
        .map(str::to_owned)
}

/// What one walk over the contract found: each place that says a leaving word.
struct AgentReadTextWalk<'contract> {
    schemas: &'contract Map<String, Value>,
    schemas_read: BTreeSet<String>,
    findings: Vec<String>,
}

impl<'contract> AgentReadTextWalk<'contract> {
    fn new(schemas: &'contract Map<String, Value>) -> Self {
        Self {
            schemas,
            schemas_read: BTreeSet::new(),
            findings: Vec::new(),
        }
    }

    fn read_text(&mut self, place: &str, text: &str) {
        if let Some(word) = the_leaving_word_in(text) {
            self.findings.push(format!("{place} says `{word}`: {text}"));
        }
    }

    /// Reads a schema's descriptions and property names, following each `$ref` once, and
    /// skipping every schema that states `x-removed-in` with all it holds.
    fn read_schema(&mut self, place: &str, schema: &Value) {
        match schema {
            Value::Object(object) => {
                if object.contains_key("x-removed-in") {
                    return;
                }
                for (key, value) in object {
                    match key.as_str() {
                        "$ref" => {
                            let Some(name) = value
                                .as_str()
                                .and_then(|reference| reference.rsplit('/').next())
                            else {
                                continue;
                            };
                            if self.schemas_read.insert(name.to_owned())
                                && let Some(referenced) = self.schemas.get(name)
                            {
                                self.read_schema(&format!("schema {name}"), referenced);
                            }
                        }
                        "description" => {
                            if let Some(text) = value.as_str() {
                                self.read_text(place, text);
                            }
                        }
                        "properties" => {
                            for (property_name, property_schema) in
                                value.as_object().into_iter().flatten()
                            {
                                if property_schema.get("x-removed-in").is_some() {
                                    continue;
                                }
                                let property_place = format!("{place}.{property_name}");
                                self.read_text(&property_place, property_name);
                                self.read_schema(&property_place, property_schema);
                            }
                        }
                        "enum" | "const" => {
                            let texts: Vec<&str> = match value {
                                Value::String(text) => vec![text.as_str()],
                                Value::Array(items) => {
                                    items.iter().filter_map(Value::as_str).collect()
                                }
                                _ => Vec::new(),
                            };
                            for text in texts {
                                self.read_text(place, text);
                            }
                        }
                        "default" | "examples" | "title" => {}
                        _ => self.read_schema(place, value),
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.read_schema(place, item);
                }
            }
            _ => {}
        }
    }

    /// Reads an example's keys and strings. A key `marked_property_names` holds is passed over
    /// with what it holds in an answer or a refusal, and is itself a finding in params, which an
    /// agent copies.
    fn read_example(
        &mut self,
        place: &str,
        example: &Value,
        kind: ExampleKind,
        marked_property_names: &BTreeSet<String>,
    ) {
        match example {
            Value::Object(object) => {
                for (key, value) in object {
                    let key_place = format!("{place}.{key}");
                    if marked_property_names.contains(key) {
                        match kind {
                            ExampleKind::AnswerOrRefusal => continue,
                            ExampleKind::Params => self.findings.push(format!(
                                "{key_place} sets a field the contract marks `x-removed-in`"
                            )),
                        }
                    }
                    self.read_text(&key_place, key);
                    self.read_example(&key_place, value, kind, marked_property_names);
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.read_example(place, item, kind, marked_property_names);
                }
            }
            Value::String(text) => self.read_text(place, text),
            _ => {}
        }
    }
}

/// What an example shows: params an agent copies, or what the runtime answers.
#[derive(Clone, Copy)]
enum ExampleKind {
    Params,
    AnswerOrRefusal,
}

/// Every property name the contract marks `x-removed-in`, wherever it sits.
fn the_property_names_marked_as_leaving(node: &Value, marked: &mut BTreeSet<String>) {
    match node {
        Value::Object(object) => {
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for (name, schema) in properties {
                    if schema.get("x-removed-in").is_some() {
                        marked.insert(name.clone());
                    }
                }
            }
            for value in object.values() {
                the_property_names_marked_as_leaving(value, marked);
            }
        }
        Value::Array(items) => {
            for item in items {
                the_property_names_marked_as_leaving(item, marked);
            }
        }
        _ => {}
    }
}

fn the_places_an_agent_reads_a_leaving_word() -> Vec<String> {
    let contract = control_contract_document();
    let schemas = contract["components"]["schemas"]
        .as_object()
        .expect("the contract's component schemas");
    let mut marked_property_names = BTreeSet::new();
    the_property_names_marked_as_leaving(&contract, &mut marked_property_names);
    let mut walk = AgentReadTextWalk::new(schemas);

    walk.read_text("the instructions", LOCAL_API_MCP_SERVER_INSTRUCTIONS);
    for method in contract["methods"].as_array().expect("the methods") {
        if method.get("x-replaced-by").is_some() {
            continue;
        }
        let name = method["name"].as_str().expect("a method name");
        let place = format!("method {name}");
        walk.read_text(&place, name);
        for field in ["summary", "description"] {
            walk.read_text(
                &format!("{place} {field}"),
                method[field].as_str().unwrap_or(""),
            );
        }
        walk.read_schema(&format!("{place} params"), &method["params"]);
        walk.read_schema(&format!("{place} result"), &method["result"]);
        walk.read_schema(&format!("{place} errors"), &method["errors"]);
        for example in method["examples"].as_array().into_iter().flatten() {
            walk.read_example(
                &format!("{place} example params"),
                &example["params"],
                ExampleKind::Params,
                &marked_property_names,
            );
            walk.read_example(
                &format!("{place} example answer"),
                &example["result"],
                ExampleKind::AnswerOrRefusal,
                &marked_property_names,
            );
        }
        if let Some(refusal) = method.get("x-refusal-example") {
            walk.read_example(
                &format!("{place} example refusal"),
                refusal,
                ExampleKind::AnswerOrRefusal,
                &marked_property_names,
            );
        }
    }
    for entry in contract["x-notifications"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            contract["x-agent-discovery-tools"]
                .as_array()
                .into_iter()
                .flatten(),
        )
    {
        let name = entry["name"].as_str().expect("an entry name");
        let place = name.to_string();
        walk.read_text(&place, name);
        for field in ["summary", "description"] {
            if let Some(text) = entry[field].as_str() {
                walk.read_text(&format!("{place} {field}"), text);
            }
        }
        for field in ["params", "result"] {
            walk.read_schema(&format!("{place} {field}"), &entry[field]);
        }
    }
    walk.findings
}

/// Mental revert: write "config" into any operation's summary that states no `x-replaced-by`,
/// or take `x-removed-in` off `graph`'s `config_checksum`, and this names that place.
#[test]
fn nothing_an_agent_reads_says_config_family_or_a_family_name_outside_what_is_leaving() {
    let findings = the_places_an_agent_reads_a_leaving_word();
    assert!(
        findings.is_empty(),
        "{} places an agent reads say a word of the forms leaving the contract:\n{}",
        findings.len(),
        findings.join("\n")
    );
}

#[test]
fn the_leaving_words_are_config_family_and_a_capitalised_family_name() {
    assert_eq!(
        the_leaving_word_in("the node's config").as_deref(),
        Some("config")
    );
    assert_eq!(
        the_leaving_word_in("exposed_config_fields").as_deref(),
        Some("config")
    );
    assert_eq!(
        the_leaving_word_in("its port families").as_deref(),
        Some("famil")
    );
    assert_eq!(
        the_leaving_word_in("one Image input").as_deref(),
        Some("Image")
    );
    assert_eq!(
        the_leaving_word_in("a Records port").as_deref(),
        Some("Records")
    );
    assert_eq!(the_leaving_word_in("a project's log records"), None);
    assert_eq!(the_leaving_word_in("an array of numbers"), None);
    assert_eq!(the_leaving_word_in("a port's pin_type"), None);
}
