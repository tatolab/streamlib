// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab_view.json`, view contract 1.0, mirroring the view SDK's Zod schema
//! (`tatolabViewManifestSchema` in tatolab-app's `tato-app-view-sdk`), which stays the guarantee.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use streamlib_processor_schema::PortFamily;

/// The view contract this runtime reads: a manifest for a later minor, or another major, does not open.
pub const VIEW_CONTRACT_READ_MAJOR: u64 = 1;
/// The highest minor of [`VIEW_CONTRACT_READ_MAJOR`] this runtime reads.
pub const VIEW_CONTRACT_READ_MINOR: u64 = 0;

/// What `tatolab_view.json` declares, defaults filled in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TatolabViewManifest {
    /// The view contract the view was built for, as `"1.0"`.
    pub contract: String,
    /// The name the view-type menu lists.
    pub title: String,
    /// What the view is for.
    pub description: Option<String>,
    /// An SVG or PNG inside the view's folder.
    pub icon: Option<String>,
    /// A picture of the view inside its folder, for a listing of views.
    pub preview: Option<String>,
    /// The HTML file the view's frame opens on, inside its folder.
    pub entry: String,
    /// Whether the view is a panel of its own or is shown on a node.
    pub placement: TatolabViewPlacement,
    /// The inputs a `node` view declares, as a node declares a port.
    #[serde(default)]
    pub inputs: Vec<TatolabViewDeclaredPort>,
    /// The outputs a `node` view declares, as a node declares a port.
    #[serde(default)]
    pub outputs: Vec<TatolabViewDeclaredPort>,
    /// The files a view takes when one is dropped on it.
    #[serde(default)]
    pub accepts: TatolabViewAcceptedFiles,
    /// The JSON Schema of what a view's area remembers, every property giving its `default`.
    pub settings: Option<Value>,
    /// Whether the view runs on an opaque origin, served only its `capabilities` and `network`.
    #[serde(default)]
    pub sandbox: bool,
    /// The parts of the view contract a sandboxed view asks for.
    #[serde(default)]
    pub capabilities: Vec<TatolabViewCapability>,
    /// The hosts a sandboxed view may reach.
    #[serde(default)]
    pub network: Vec<String>,
}

/// Where a view is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TatolabViewPlacement {
    /// A panel with no node, opened from any area.
    Area,
    /// A panel shown on a node of its type, reading that node's inputs and writing its outputs.
    Node,
}

impl TatolabViewPlacement {
    /// The name `tatolab_view.json` spells this placement by.
    pub fn manifest_name(self) -> &'static str {
        match self {
            TatolabViewPlacement::Area => "area",
            TatolabViewPlacement::Node => "node",
        }
    }
}

/// An input or output a `node` view declares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct TatolabViewDeclaredPort {
    /// The port's name: lower-case letters, digits and underscores.
    pub name: String,
    /// The one family the port carries.
    pub family: PortFamily,
    /// What the port carries.
    pub description: Option<String>,
    /// The lowest and highest value an Array port carries, lowest first.
    pub range: Option<[f64; 2]>,
}

/// The files a view takes, by extension.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TatolabViewAcceptedFiles {
    /// Each extension, lower case with its dot, as `.glb`.
    #[serde(default)]
    pub files: Vec<String>,
}

/// A part of the view contract a sandboxed view may ask for beyond what every view gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum TatolabViewCapability {
    #[serde(rename = "read.selection")]
    ReadSelection,
    #[serde(rename = "write.selection")]
    WriteSelection,
    #[serde(rename = "read.stream_graph")]
    ReadStreamGraph,
    #[serde(rename = "read.project_assets")]
    ReadProjectAssets,
    #[serde(rename = "write.project_assets")]
    WriteProjectAssets,
    #[serde(rename = "edit.stream_graph")]
    EditStreamGraph,
    #[serde(rename = "gpu")]
    Gpu,
    #[serde(rename = "camera")]
    Camera,
    #[serde(rename = "microphone")]
    Microphone,
}

/// One way a manifest falls short of the view contract: the field it names and why.
struct TatolabViewManifestIssue {
    field_path: String,
    why: String,
}

/// `manifest_json`, a view's `tatolab_view.json` as parsed, read against view contract 1.0, or
/// every way it falls short, each as "`field`: why.", as the view SDK words them.
pub fn read_tatolab_view_manifest(manifest_json: &Value) -> Result<TatolabViewManifest, String> {
    let mut manifest: TatolabViewManifest = serde_json::from_value(manifest_json.clone())
        .map_err(|not_the_contract| format!("`tatolab_view.json`: {not_the_contract}."))?;
    let issues = every_issue_of_a_manifest(&manifest);
    if !issues.is_empty() {
        return Err(issues
            .iter()
            .map(|issue| format!("`{}`: {}.", issue.field_path, issue.why))
            .collect::<Vec<_>>()
            .join(" "));
    }
    let mut capabilities_seen = BTreeSet::new();
    manifest
        .capabilities
        .retain(|capability| capabilities_seen.insert(*capability));
    Ok(manifest)
}

fn every_issue_of_a_manifest(manifest: &TatolabViewManifest) -> Vec<TatolabViewManifestIssue> {
    let mut issues = Vec::new();
    let mut issue = |field_path: String, why: &str| {
        issues.push(TatolabViewManifestIssue {
            field_path,
            why: why.to_owned(),
        })
    };
    if !is_a_contract_version(&manifest.contract) {
        issue(
            "contract".into(),
            "it names the view contract the view was built for, as \"1.0\"",
        );
    } else if !is_a_contract_version_this_runtime_reads(&manifest.contract) {
        issue(
            "contract".into(),
            &format!(
                "this runtime reads view contract {VIEW_CONTRACT_READ_MAJOR}.{VIEW_CONTRACT_READ_MINOR}"
            ),
        );
    }
    if manifest.title.trim().is_empty() {
        issue("title".into(), "it is the name the view-type menu lists");
    }
    for (field, path) in [
        ("icon", manifest.icon.as_deref()),
        ("preview", manifest.preview.as_deref()),
        ("entry", Some(manifest.entry.as_str())),
    ] {
        if let Some(path) = path
            && !is_a_path_inside_the_views_folder(path)
        {
            issue(
                field.into(),
                "it is a path inside the view's folder, with no `..` step, scheme or backslash",
            );
        }
    }
    for (direction, ports) in [("inputs", &manifest.inputs), ("outputs", &manifest.outputs)] {
        let mut names_seen = BTreeSet::new();
        for (index, port) in ports.iter().enumerate() {
            if !is_a_port_name(&port.name) {
                issue(
                    format!("{direction}[{index}].name"),
                    "a port name is lower-case letters, digits and underscores",
                );
            }
            if !names_seen.insert(port.name.as_str()) {
                issue(
                    format!("{direction}[{index}].name"),
                    &format!("`{}` is named twice", port.name),
                );
            }
            if let Some([lowest, highest]) = port.range {
                if lowest >= highest {
                    issue(
                        format!("{direction}[{index}].range"),
                        "a range is [lowest, highest], lowest first",
                    );
                }
                if port.family != PortFamily::Array {
                    issue(
                        format!("{direction}[{index}].range"),
                        "only an Array port carries a range",
                    );
                }
            }
        }
    }
    for (index, extension) in manifest.accepts.files.iter().enumerate() {
        if !is_a_file_extension(extension) {
            issue(
                format!("accepts.files[{index}]"),
                "an extension is lower case with its dot, as \".glb\"",
            );
        }
    }
    if let Some(settings) = &manifest.settings
        && let Some((field_path, why)) = why_a_settings_declaration_is_not_one(settings)
    {
        issue(field_path, why);
    }
    for (index, host) in manifest.network.iter().enumerate() {
        if !is_a_network_host(host) {
            issue(
                format!("network[{index}]"),
                "a host is a lower-case name, with a port if it needs one, and no scheme or wildcard",
            );
        }
    }
    if manifest.placement == TatolabViewPlacement::Area
        && (!manifest.inputs.is_empty() || !manifest.outputs.is_empty())
    {
        issue(
            "placement".into(),
            "an `area` view has no node, so it declares no inputs or outputs; a view with ports has `node` placement",
        );
    }
    issues
}

fn is_a_canonical_unsigned_integer(text: &str) -> bool {
    text == "0"
        || (!text.is_empty()
            && !text.starts_with('0')
            && text.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_a_contract_version(contract: &str) -> bool {
    contract.split_once('.').is_some_and(|(major, minor)| {
        is_a_canonical_unsigned_integer(major) && is_a_canonical_unsigned_integer(minor)
    })
}

fn is_a_contract_version_this_runtime_reads(contract: &str) -> bool {
    let Some((major, minor)) = contract.split_once('.') else {
        return false;
    };
    let (Ok(major), Ok(minor)) = (major.parse::<u64>(), minor.parse::<u64>()) else {
        return false;
    };
    major == VIEW_CONTRACT_READ_MAJOR
        && (major, minor) <= (VIEW_CONTRACT_READ_MAJOR, VIEW_CONTRACT_READ_MINOR)
}

/// A path inside the view's own folder: relative, with no `..` step, no scheme and no backslash.
pub fn is_a_path_inside_the_views_folder(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || starts_with_a_scheme(path)
    {
        return false;
    }
    path.split('/')
        .all(|step| !step.is_empty() && step != "." && step != "..")
}

fn starts_with_a_scheme(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-')
        })
}

fn is_a_port_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == '_')
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn is_a_file_extension(extension: &str) -> bool {
    extension.strip_prefix('.').is_some_and(|after_the_dot| {
        !after_the_dot.is_empty()
            && after_the_dot
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    })
}

fn is_a_host_label(label: &str) -> bool {
    !label.is_empty()
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

fn is_a_network_host(host: &str) -> bool {
    let (name, port) = match host.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    if let Some(port) = port
        && (!(1..=5).contains(&port.len()) || !port.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    (1..=253).contains(&name.len()) && name.split('.').all(is_a_host_label)
}

/// The field and why when `settings` is not the JSON Schema of an object whose every property
/// gives its `default`.
fn why_a_settings_declaration_is_not_one(settings: &Value) -> Option<(String, &'static str)> {
    let Some(declaration) = settings.as_object() else {
        return Some(("settings".into(), "it is a JSON Schema object"));
    };
    if declaration.get("type") != Some(&Value::String("object".into())) {
        return Some(("settings.type".into(), "it is \"object\""));
    }
    let Some(properties) = declaration.get("properties").and_then(Value::as_object) else {
        return Some((
            "settings.properties".into(),
            "it is an object of each setting's JSON Schema",
        ));
    };
    for (setting_name, property) in properties {
        if !property
            .as_object()
            .is_some_and(|property| property.contains_key("default"))
        {
            return Some((
                format!("settings.properties.{setting_name}.default"),
                "every setting gives its `default`",
            ));
        }
    }
    if declaration
        .get("additionalProperties")
        .is_some_and(|additional_properties| !additional_properties.is_boolean())
    {
        return Some(("settings.additionalProperties".into(), "it is a boolean"));
    }
    None
}

#[cfg(test)]
#[path = "tatolab_view_manifest_tests.rs"]
mod tatolab_view_manifest_tests;
