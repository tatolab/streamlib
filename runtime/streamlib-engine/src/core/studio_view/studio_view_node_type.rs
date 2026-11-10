// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    TatolabViewDeclaredPort, TatolabViewManifest, TatolabViewPlacement,
    every_view_a_project_and_its_imports_bring,
};

use super::studio_view_processor::StudioViewProcessor;
use crate::core::descriptors::{
    NodeKind, PortDescriptor, PortFamilyDeclaration, ProcessorClassImportPath,
    ProcessorClassShortName, ProcessorDescriptor,
};
use crate::core::error::{Error, Result};
use crate::core::graph::{PortInfo, ProcessorNode, names_listed_for_a_refusal};
use crate::core::processors::{
    DynGeneratedProcessor, HostCompiledRustTypeRegistration, NodeConfigTakenCheckFn,
    NodeWhosePortsItsTypeResolves, PortsEachNodeOfItsTypeTakesFn, ProcessorInstanceFactory,
};

/// The node type a Tato Studio view on the canvas is.
pub const STUDIO_VIEW_NODE_TYPE: &str = "tatolab.studio:StudioView";

const STUDIO_VIEW_SHORT_NAME: &str = "StudioView";

/// The delivery profile every input of a StudioView node declares: a panel reads its latest value.
const STUDIO_VIEW_INPUT_DELIVERY_PROFILE: &str = "newest";

/// The type [`STUDIO_VIEW_NODE_TYPE`] names.
pub fn the_studio_view_node_type() -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(STUDIO_VIEW_NODE_TYPE)
        .expect("the StudioView node type is a class import path")
}

/// Whether `node_type` is the StudioView node type.
pub fn is_the_studio_view_node_type(node_type: &ProcessorClassImportPath) -> bool {
    node_type.as_str() == STUDIO_VIEW_NODE_TYPE
}

/// A StudioView node's config: the view it shows, and that view's settings beside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StudioViewNodeConfig {
    /// The view the node shows, `<project name>:<view folder>`, as `list_project_views` lists it.
    pub view_type: String,
    /// The view's settings, each by the name its manifest's `settings` declares.
    #[serde(flatten)]
    pub settings: Map<String, Value>,
}

impl StudioViewNodeConfig {
    /// `config` as the config of the StudioView node `node_name`, or the refusal naming the node.
    pub(crate) fn of_the_node(node_name: &str, config: &Value) -> Result<Self> {
        serde_path_to_error::deserialize(config).map_err(|refusal| Error::NodeConfigRefused {
            node_name: node_name.to_string(),
            node_type: the_studio_view_node_type(),
            refusal: refusal.to_string(),
        })
    }
}

/// The JSON Schema of a [`StudioViewNodeConfig`].
fn the_studio_view_config_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "StudioViewNodeConfig",
        "type": "object",
        "properties": {
            "view_type": {
                "description": "The view the node shows, `<project name>:<view folder>`, as \
                                `list_project_views` lists it.",
                "type": "string"
            }
        },
        "required": ["view_type"],
        "additionalProperties": true
    })
}

fn the_studio_view_descriptor() -> ProcessorDescriptor {
    let mut descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new(STUDIO_VIEW_SHORT_NAME)
            .expect("`StudioView` is a class short name"),
        the_studio_view_node_type(),
        "A Tato Studio view on the canvas. Its inputs and outputs are the ones the view's \
         `tatolab_view.json` declares, each of its declared family, read from the project and \
         its imports when the node loads; its other config keys are the view's settings. A panel \
         reads an input through `open_viewer` on the port linked into it, and writes an output \
         with `write_studio_view_output`, which the node publishes on that port. A written value \
         is live data: nothing written is recorded.",
    )
    .with_config_schema(the_studio_view_config_schema());
    descriptor.node_kind = NodeKind::Process;
    descriptor
}

/// The view `view_type` of the project in `project_directory` or one of its imports, read
/// against the view contract, or why a node cannot show it.
fn the_manifest_of_the_view(
    project_directory: Option<&Path>,
    view_type: &str,
) -> std::result::Result<TatolabViewManifest, String> {
    let Some(project_directory) = project_directory else {
        return Err("the node's stream loads from no project, so it brings no view".to_string());
    };
    let views = every_view_a_project_and_its_imports_bring(project_directory)
        .map_err(|refusal| format!("the project's views do not list: {refusal}"))?;
    let Some(view) = views.iter().find(|view| view.view_type == view_type) else {
        return Err(format!(
            "the project and its imports bring no view `{view_type}`. They bring: {}",
            names_listed_for_a_refusal(
                views.iter().map(|view| format!("`{}`", view.view_type)),
                "no view",
            )
        ));
    };
    let manifest = view.manifest.clone()?;
    if manifest.placement == TatolabViewPlacement::Area {
        return Err(
            "it is an `area` view, a panel with no node; a node shows a `node` view".to_string(),
        );
    }
    Ok(manifest)
}

fn the_port_a_view_declares(
    declared: &TatolabViewDeclaredPort,
    is_an_input: bool,
) -> PortDescriptor {
    let port = PortDescriptor::new(
        &declared.name,
        declared.description.clone().unwrap_or_default(),
        false,
        PortFamilyDeclaration::of_family(declared.family),
    );
    if is_an_input {
        port.with_delivery_profile(STUDIO_VIEW_INPUT_DELIVERY_PROFILE)
    } else {
        port
    }
}

/// The input and output ports a StudioView node takes: one per input and output its view's
/// manifest declares, each of its family.
fn the_ports_of_a_studio_view_node(
    node: &NodeWhosePortsItsTypeResolves<'_>,
) -> Result<(Vec<PortInfo>, Vec<PortInfo>)> {
    let config = StudioViewNodeConfig::of_the_node(node.node_name, node.config)?;
    let manifest =
        the_manifest_of_the_view(node.project_directory, &config.view_type).map_err(|why| {
            Error::StudioViewDoesNotOpen {
                node_name: node.node_name.to_string(),
                view_type: config.view_type.clone(),
                why,
            }
        })?;
    let inputs = manifest
        .inputs
        .iter()
        .map(|declared| PortInfo::from(&the_port_a_view_declares(declared, true)))
        .collect();
    let outputs = manifest
        .outputs
        .iter()
        .map(|declared| PortInfo::from(&the_port_a_view_declares(declared, false)))
        .collect();
    Ok((inputs, outputs))
}

/// The descriptor of the StudioView node `node`: the type's, carrying the ports the node took.
pub(super) fn the_descriptor_of_a_studio_view_node(node: &ProcessorNode) -> ProcessorDescriptor {
    let port_descriptor_of = |port: &PortInfo| {
        let mut descriptor =
            PortDescriptor::new(&port.name, &port.description, false, port.family.clone());
        descriptor.delivery_profile = port.delivery_profile.clone();
        descriptor
    };
    let mut descriptor = the_studio_view_descriptor();
    descriptor.inputs = node.ports.inputs.iter().map(port_descriptor_of).collect();
    descriptor.outputs = node.ports.outputs.iter().map(port_descriptor_of).collect();
    descriptor
}

/// Register the StudioView node type on `factory`.
pub(crate) fn register_the_studio_view_node_type(factory: &ProcessorInstanceFactory) {
    let constructor = Box::new(
        |node: &ProcessorNode| -> Result<Box<dyn DynGeneratedProcessor + Send>> {
            Ok(Box::new(StudioViewProcessor::of_the_node(node)?))
        },
    );
    let config_taken_check: NodeConfigTakenCheckFn = Box::new(|node_name, _node_type, config| {
        StudioViewNodeConfig::of_the_node(node_name, config).map(drop)
    });
    let ports_each_node_takes: PortsEachNodeOfItsTypeTakesFn =
        Box::new(the_ports_of_a_studio_view_node);
    if let Err(refusal) = factory.register_a_native_type_whose_every_node_takes_its_own_ports(
        the_studio_view_descriptor(),
        HostCompiledRustTypeRegistration {
            constructor,
            config_taken_check,
        },
        ports_each_node_takes,
    ) {
        tracing::error!(%refusal, "the StudioView node type did not register");
    }
}
