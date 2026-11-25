// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab_view.json`, view contract 1.1, read as the view SDK's Zod schema
//! (`tatolabViewManifestSchema` in tatolab-app's `tato-app-view-sdk`) reads it. That schema is the
//! contract; `view-contract/tatolab_view_contract_examples.json` holds both to the same answers.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use streamlib_node_schema::PortFamily;
use streamlib_node_schema::pin_type::{BundleMessage, PinType, built_in_pin_type};

use crate::manifest_fields::{
    a_value_never_written_as_null, is_a_contract_version,
    is_a_contract_version_of_major_up_to_minor, is_a_path_inside_its_own_folder,
    the_field_a_serde_refusal_names,
};
use crate::project_views::TATOLAB_VIEW_MANIFEST_FILE_NAME;

/// The view contract this runtime reads: a manifest for a later minor, or another major, does not open.
pub const VIEW_CONTRACT_READ_MAJOR: u64 = 1;
/// The highest minor of [`VIEW_CONTRACT_READ_MAJOR`] this runtime reads.
pub const VIEW_CONTRACT_READ_MINOR: u64 = 1;
/// The first minor of [`VIEW_CONTRACT_READ_MAJOR`] in which a declared port names its `pin_type`.
const VIEW_CONTRACT_MINOR_OF_A_PORTS_PIN_TYPE: u64 = 1;

/// The families of the outputs a panel writes: each value of one is set on the view node's input
/// of the output's name, and published on the output.
pub const FAMILIES_A_PANEL_WRITES: [PortFamily; 3] =
    [PortFamily::Array, PortFamily::Channels, PortFamily::Records];

/// Whether a panel writes an output of `family`.
pub fn a_panel_writes(family: PortFamily) -> bool {
    FAMILIES_A_PANEL_WRITES.contains(&family)
}

/// What `tatolab_view.json` declares, defaults filled in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TatolabViewManifest {
    /// The view contract the view was built for, as `"1.0"`.
    pub contract: String,
    /// The view's own UUID, which a node placing the view is typed by; the runtime writes it
    /// into a project's view that has none the first time the project opens.
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    pub view: Option<uuid::Uuid>,
    /// The name the view-type menu lists.
    pub title: String,
    /// What the view is for.
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    pub description: Option<String>,
    /// An SVG or PNG inside the view's folder.
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    pub icon: Option<String>,
    /// A picture of the view inside its folder, for a listing of views.
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
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
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    pub settings: Option<Value>,
    /// What the view declares it uses, which Studio hands the view and asks the system for.
    #[serde(default)]
    pub capabilities: Vec<TatolabViewCapability>,
}

/// Where a view is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// An input or output a `node` view declares, by its pin type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TatolabViewDeclaredPortAsWritten")]
pub struct TatolabViewDeclaredPort {
    /// The port's name: lower-case letters, digits and underscores.
    pub name: String,
    /// The one family the messages of the port's pin type belong to, which a reader of the
    /// contract's earlier versions still reads beside the pin type.
    pub family: PortFamily,
    /// The pin type the port carries.
    pub pin_type: PinType,
    /// What the port carries.
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    pub description: Option<String>,
    /// The lowest and highest value an Array port carries, lowest first.
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    pub range: Option<[f64; 2]>,
}

/// A declared port as `tatolab_view.json` writes it, the older `family` field read so that a
/// port still declared by it is refused naming the `pin_type` to write.
#[derive(Deserialize)]
struct TatolabViewDeclaredPortAsWritten {
    name: String,
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    family: Option<PortFamily>,
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    pin_type: Option<PinType>,
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    description: Option<String>,
    #[serde(default, deserialize_with = "a_value_never_written_as_null")]
    range: Option<[f64; 2]>,
}

impl TryFrom<TatolabViewDeclaredPortAsWritten> for TatolabViewDeclaredPort {
    type Error = String;

    fn try_from(port: TatolabViewDeclaredPortAsWritten) -> Result<Self, String> {
        let name = &port.name;
        let pin_type = match (port.pin_type, port.family) {
            (None, None) => {
                return Err(format!("port `{name}` names the `pin_type` it carries"));
            }
            (None, Some(family)) => {
                return Err(the_words_of_a_port_declared_by_family(name, family));
            }
            (Some(_), Some(_)) => {
                return Err(format!(
                    "port `{name}` names the older `family` field beside its `pin_type`; drop \
                     `family`, which the `pin_type` sets"
                ));
            }
            (Some(pin_type), None) => pin_type,
        };
        pin_type
            .validate()
            .map_err(|why| format!("`pin_type`: {why}"))?;
        let Some(family) = PortFamily::carrying_a_whole_value_of(&pin_type) else {
            return Err(format!(
                "`pin_type`: a {pin_type} rides no message a view's port reads or writes; a \
                 view declares a port of a type a link carries"
            ));
        };
        Ok(Self {
            name: port.name,
            family,
            pin_type,
            description: port.description,
            range: port.range,
        })
    }
}

/// Why port `name`, declared by the older `family` field as `family`, does not open, and what
/// it writes in its place: the one pin type the family's messages carry where there is one, as
/// an Image port's Texture; else the types it may become, none picked, since a guessed type can
/// refuse a link that works today.
fn the_words_of_a_port_declared_by_family(name: &str, family: PortFamily) -> String {
    let family_name = family.wire_name();
    let to_write = format!(
        "in place of `\"family\": \"{family_name}\"`, with `\"contract\": \
         \"{VIEW_CONTRACT_READ_MAJOR}.{VIEW_CONTRACT_MINOR_OF_A_PORTS_PIN_TYPE}\"`"
    );
    let pin_types: Vec<&str> = family
        .the_pin_types_it_carries()
        .into_iter()
        .map(|pin_type| built_in_pin_type(pin_type).typescript_form)
        .collect();
    let bundle_advice = if BundleMessage::RecordsMap.its_family() == Some(family) {
        ", or a `bundle` of small members, as a record of named numbers is, written \
         `{\"type\": \"bundle\", \"members\": [{\"name\": \"<name>\", \
         \"member_type\": {\"type\": \"float\"}}, ...]}`"
    } else {
        ""
    };
    match pin_types.as_slice() {
        [pin_type] => format!(
            "port `{name}` is declared by the older `family` field, which a view's port no \
             longer names; write `\"pin_type\": {{\"type\": {pin_type}}}` {to_write}"
        ),
        _ => format!(
            "port `{name}` is declared by the older `family` field, which a view's port no \
             longer names; a `{family_name}` port carries one of {}{bundle_advice}: write the \
             one it carries as its `pin_type`, as `{{\"type\": \"<type>\"}}`, {to_write}",
            pin_types
                .iter()
                .map(|pin_type| format!("`{}`", pin_type.trim_matches('"')))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The files a view takes, by extension.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TatolabViewAcceptedFiles {
    /// Each extension, lower case with its dot, as `.glb`.
    #[serde(default)]
    pub files: Vec<String>,
}

/// A part of the view contract, or a device, a view declares it uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// One way a manifest falls short of the view contract: the field it names, why, and the
/// declared port that field lies in, by its name, when it lies in one.
struct TatolabViewManifestIssue {
    field_path: String,
    why: String,
    port: Option<String>,
}

/// Why a view's `tatolab_view.json` does not open: every way it falls short, and the name of the
/// declared port the first of them that is about a port is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TatolabViewManifestRefusal {
    /// Each way the manifest falls short, as "`field`: why.", as the view SDK words them.
    pub(crate) words: String,
    /// The declared port a refusal of one of its fields is about, by its name.
    pub(crate) port: Option<String>,
}

/// `manifest_json`, a view's `tatolab_view.json` as parsed, read against view contract 1.1, or
/// every way it falls short, each as "`field`: why.", as the view SDK words them.
pub fn read_tatolab_view_manifest(manifest_json: &Value) -> Result<TatolabViewManifest, String> {
    read_tatolab_view_manifest_naming_the_port_it_refuses(manifest_json)
        .map_err(|refusal| refusal.words)
}

/// [`read_tatolab_view_manifest`], its refusal naming the declared port it is about.
pub(crate) fn read_tatolab_view_manifest_naming_the_port_it_refuses(
    manifest_json: &Value,
) -> Result<TatolabViewManifest, TatolabViewManifestRefusal> {
    let mut manifest: TatolabViewManifest = serde_path_to_error::deserialize(manifest_json.clone())
        .map_err(|not_the_contract| {
            let field_path =
                the_field_a_serde_refusal_names(&not_the_contract, TATOLAB_VIEW_MANIFEST_FILE_NAME);
            TatolabViewManifestRefusal {
                port: the_declared_port_a_path_lies_in(manifest_json, not_the_contract.path()),
                words: format!("`{field_path}`: {}.", not_the_contract.into_inner()),
            }
        })?;
    let issues = every_issue_of_a_manifest(&manifest);
    if !issues.is_empty() {
        return Err(TatolabViewManifestRefusal {
            words: issues
                .iter()
                .map(|issue| format!("`{}`: {}.", issue.field_path, issue.why))
                .collect::<Vec<_>>()
                .join(" "),
            port: issues.iter().find_map(|issue| issue.port.clone()),
        });
    }
    manifest.title = manifest.title.trim().to_owned();
    let mut capabilities_seen = BTreeSet::new();
    manifest
        .capabilities
        .retain(|capability| capabilities_seen.insert(*capability));
    Ok(manifest)
}

/// The name of the declared port of `manifest_json` that `path`, where serde refused it, lies
/// in, when it lies in one that has a name: its first steps are `inputs` or `outputs`, then an
/// index.
fn the_declared_port_a_path_lies_in(
    manifest_json: &Value,
    path: &serde_path_to_error::Path,
) -> Option<String> {
    let mut segments = path.iter();
    let serde_path_to_error::Segment::Map { key: direction } = segments.next()? else {
        return None;
    };
    let serde_path_to_error::Segment::Seq { index } = segments.next()? else {
        return None;
    };
    if direction != "inputs" && direction != "outputs" {
        return None;
    }
    manifest_json
        .get(direction)?
        .get(*index)?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

fn every_issue_of_a_manifest(manifest: &TatolabViewManifest) -> Vec<TatolabViewManifestIssue> {
    let mut issues = Vec::new();
    let mut issue = |field_path: String, why: &str, port: Option<&str>| {
        issues.push(TatolabViewManifestIssue {
            field_path,
            why: why.to_owned(),
            port: port.map(str::to_owned),
        })
    };
    if !is_a_contract_version(&manifest.contract) {
        issue(
            "contract".into(),
            "it names the view contract the view was built for, as \"1.0\"",
            None,
        );
    } else if !is_a_contract_version_this_runtime_reads(&manifest.contract) {
        issue(
            "contract".into(),
            &format!(
                "this runtime reads view contract {VIEW_CONTRACT_READ_MAJOR}.{VIEW_CONTRACT_READ_MINOR}"
            ),
            None,
        );
    }
    if manifest.title.trim().is_empty() {
        issue(
            "title".into(),
            "it is the name the view-type menu lists",
            None,
        );
    }
    for (field, path) in [
        ("icon", manifest.icon.as_deref()),
        ("preview", manifest.preview.as_deref()),
        ("entry", Some(manifest.entry.as_str())),
    ] {
        if let Some(path) = path
            && !is_a_path_inside_its_own_folder(path)
        {
            issue(
                field.into(),
                "it is a path inside the view's folder, with no `..` step, scheme or backslash",
                None,
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
                    Some(&port.name),
                );
            }
            if !names_seen.insert(port.name.as_str()) {
                issue(
                    format!("{direction}[{index}].name"),
                    &format!("`{}` is named twice", port.name),
                    Some(&port.name),
                );
            }
            if let Some([lowest, highest]) = port.range {
                if lowest >= highest {
                    issue(
                        format!("{direction}[{index}].range"),
                        "a range is [lowest, highest], lowest first",
                        Some(&port.name),
                    );
                }
                if port.family != PortFamily::Array {
                    issue(
                        format!("{direction}[{index}].range"),
                        "only an Array port carries a range",
                        Some(&port.name),
                    );
                }
            }
        }
    }
    let first_port = ["inputs", "outputs"]
        .into_iter()
        .zip([&manifest.inputs, &manifest.outputs])
        .find_map(|(direction, ports)| Some((direction, ports.first()?)));
    if let Some((direction, port)) = first_port
        && !names_a_minor_with_a_ports_pin_type(&manifest.contract)
    {
        issue(
            format!("{direction}[0].pin_type"),
            &format!(
                "a port's `pin_type` is view contract \
                 {VIEW_CONTRACT_READ_MAJOR}.{VIEW_CONTRACT_MINOR_OF_A_PORTS_PIN_TYPE}; write \
                 `\"contract\": \"{VIEW_CONTRACT_READ_MAJOR}.{VIEW_CONTRACT_MINOR_OF_A_PORTS_PIN_TYPE}\"`"
            ),
            Some(&port.name),
        );
    }
    for (index, output) in manifest.outputs.iter().enumerate() {
        if a_panel_writes(output.family) && !is_a_type_a_panel_writes_a_value_of(&output.pin_type) {
            issue(
                format!("outputs[{index}].pin_type"),
                &format!(
                    "a panel writes a value onto a `{}` output, and a {} holds none",
                    output.family.wire_name(),
                    output.pin_type
                ),
                Some(&output.name),
            );
        }
    }
    let input_names: BTreeSet<&str> = manifest
        .inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect();
    let setting_names: BTreeSet<&str> = manifest
        .settings
        .as_ref()
        .and_then(|settings| settings.get("properties"))
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.keys().map(String::as_str))
        .collect();
    for (index, output) in manifest.outputs.iter().enumerate() {
        let name = output.name.as_str();
        let why = if input_names.contains(name) {
            format!("`{name}` names an input too, and a port is named once")
        } else if a_panel_writes(output.family) && setting_names.contains(name) {
            format!(
                "`{name}` names a setting too, and the value of an output a panel writes is set \
                 on the view node's input of the output's name, so such an output is named \
                 after no setting"
            )
        } else {
            continue;
        };
        issue(format!("outputs[{index}].name"), &why, Some(name));
    }
    for (index, extension) in manifest.accepts.files.iter().enumerate() {
        if !is_a_file_extension(extension) {
            issue(
                format!("accepts.files[{index}]"),
                "an extension is lower case with its dot, as \".glb\"",
                None,
            );
        }
    }
    if let Some(settings) = &manifest.settings
        && let Some((field_path, why)) = why_a_settings_declaration_is_not_one(settings)
    {
        issue(field_path, why, None);
    }
    if manifest.placement == TatolabViewPlacement::Area
        && (!manifest.inputs.is_empty() || !manifest.outputs.is_empty())
    {
        issue(
            "placement".into(),
            "an `area` view has no node, so it declares no inputs or outputs; a view with ports has `node` placement",
            None,
        );
    }
    issues
}

/// Whether a panel writes values of `pin_type` onto an output: one a pin of it holds, or the
/// samples of an Audio block or the numbers of a Tensor.
fn is_a_type_a_panel_writes_a_value_of(pin_type: &PinType) -> bool {
    pin_type.holds_a_stored_value() || matches!(pin_type, PinType::Audio {} | PinType::Tensor {})
}

fn is_a_contract_version_this_runtime_reads(contract: &str) -> bool {
    is_a_contract_version_of_major_up_to_minor(
        contract,
        VIEW_CONTRACT_READ_MAJOR,
        VIEW_CONTRACT_READ_MINOR,
    )
}

/// Whether `contract`, a version this runtime reads, is one in which a port names its `pin_type`.
fn names_a_minor_with_a_ports_pin_type(contract: &str) -> bool {
    !is_a_contract_version_of_major_up_to_minor(
        contract,
        VIEW_CONTRACT_READ_MAJOR,
        VIEW_CONTRACT_MINOR_OF_A_PORTS_PIN_TYPE - 1,
    )
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
