// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `node_ui/tatolab_node_ui.json`, the controls and node bodies a module registers, and the
//! answer `list_project_node_ui_elements` gives. The manifest's shape is written here once: the
//! runtime reads the file with it, the control contract renders it, and the runtime client's
//! TypeScript type and the JSON Schema an author's editor reads are generated from that.

use std::path::PathBuf;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use streamlib_node_schema::pin_type::{BuiltInPinTypeName, NamedPinTypeImportPath};

/// What a module's `node_ui/tatolab_node_ui.json` registers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeUiManifest {
    /// The view contract the elements were built for, as `"1.1"`.
    pub contract: String,
    /// Each control the module registers, which edits or shows an input of one pin type.
    #[serde(default)]
    pub controls: Vec<NodeUiManifestControl>,
    /// Each node body the module registers, which draws the inside of one node type's node.
    #[serde(default)]
    pub node_bodies: Vec<NodeUiManifestNodeBody>,
}

/// A control a module registers for one pin type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeUiManifestControl {
    /// The pin type whose inputs the control edits or shows.
    #[serde(rename = "type")]
    pub pin_type: NodeUiControlPinType,
    /// The control's name, which a pin's `control` annotation picks it by. Left out, the control
    /// is its pin type's default.
    #[serde(
        default,
        deserialize_with = "a_name_never_written_as_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "json-schema", schemars(with = "String"))]
    pub control: Option<String>,
    /// The script that default-exports the control's element class, inside the node UI folder.
    pub script: String,
}

/// The pin type a control is registered for: a built-in type by its name, as `float`, or a
/// module's named type by the import path of the class that declares it, as
/// `nodes.sensor:SensorReading`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum NodeUiControlPinType {
    /// A built-in type, whose default control only the Studio's own module registers.
    BuiltIn(BuiltInPinTypeName),
    /// A module's named type, whose default control only the module that declares it registers.
    Named(NamedPinTypeImportPath),
}

impl NodeUiControlPinType {
    /// The type as a manifest writes it, as `float` or `nodes.sensor:SensorReading`.
    pub fn as_written(&self) -> String {
        match self {
            Self::BuiltIn(name) => serde_json::to_value(name)
                .ok()
                .and_then(|name| name.as_str().map(str::to_owned))
                .unwrap_or_default(),
            Self::Named(import_path) => import_path.as_str().to_owned(),
        }
    }
}

impl<'de> Deserialize<'de> for NodeUiControlPinType {
    /// A name with a `:` between a module and a class is a named type, and any other a
    /// built-in type's name, so a misspelled built-in name is refused naming the built-in names.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let written = String::deserialize(deserializer)?;
        if written.contains(':') {
            NamedPinTypeImportPath::new(written)
                .map(Self::Named)
                .map_err(D::Error::custom)
        } else {
            BuiltInPinTypeName::deserialize(serde::de::value::StringDeserializer::<D::Error>::new(
                written,
            ))
            .map(Self::BuiltIn)
        }
    }
}

/// A node body a module registers for one node type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeUiManifestNodeBody {
    /// The node type whose node it draws, as `acme:Crossfade`.
    pub node_type: String,
    /// The script that default-exports the node body's element class, inside the node UI folder.
    pub script: String,
}

/// A control's name as written: left out, never written as `null`.
fn a_name_never_written_as_null<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| D::Error::custom("it is left out, never written as null"))
}

/// The `list_project_node_ui_elements` tool's result: the controls and node bodies the project
/// and each of its imports register, read from each one's `node_ui/tatolab_node_ui.json` with
/// none of its scripts run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListProjectNodeUiElementsToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// Each element registered, the project's own first, then each import's in the order its
    /// project file records them, each module's in the order its manifest writes them.
    pub elements: Vec<ProjectNodeUiElementListed>,
    /// Each manifest that reads, as written, in the same order.
    pub manifests: Vec<ProjectNodeUiManifestListed>,
    /// Each manifest that does not read, with its file and why; the other modules still list.
    pub unreadable_manifests: Vec<ProjectNodeUiManifestNotRead>,
}

/// The module that registers a node UI element: the listed project itself, or one of its
/// imports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectNodeUiModule {
    /// The module's project UUID.
    pub project: String,
    /// That project's name, which qualifies a control name it registers, as `acme:knob`.
    pub project_name: String,
    /// Its folder under `imports/`; `null` for the listed project's own.
    pub import_folder: Option<String>,
}

/// One control or node body a module registers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectNodeUiElementListed {
    /// The module that registers it.
    pub module: ProjectNodeUiModule,
    /// Its script, relative to the project, as `node_ui/knob.ts` or
    /// `imports/<folder>/node_ui/knob.ts`, which names it to a project file read.
    pub script_path: String,
    /// What it is, and the pin type or node type it is for.
    pub element: ProjectNodeUiElement,
}

/// What a node UI element is, under `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectNodeUiElement {
    /// A control, which edits or shows an input of one pin type.
    Control {
        /// The pin type whose inputs it edits or shows.
        pin_type: NodeUiControlPinType,
        /// Its name, which a pin's `control` annotation picks it by; `null` for its pin type's
        /// default.
        control: Option<String>,
    },
    /// A node body, which draws the inside of one node type's node.
    NodeBody {
        /// The node type whose node it draws.
        node_type: String,
    },
}

/// One module's manifest that reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectNodeUiManifestListed {
    /// The module whose manifest it is.
    pub module: ProjectNodeUiModule,
    /// Its file, relative to the project, as `node_ui/tatolab_node_ui.json` or
    /// `imports/<folder>/node_ui/tatolab_node_ui.json`.
    pub manifest_path: String,
    /// The manifest as written.
    pub manifest: NodeUiManifest,
}

/// One module's manifest that does not read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectNodeUiManifestNotRead {
    /// The module whose manifest it is.
    pub module: ProjectNodeUiModule,
    /// Its file, relative to the project.
    pub manifest_path: String,
    /// Why it does not read: it cannot be read, is not JSON, or does not read against the view
    /// contract, each as "`field`: why.".
    pub why: String,
}
