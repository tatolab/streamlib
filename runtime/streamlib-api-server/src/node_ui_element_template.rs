// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A new node UI element's script, rendered from the extension SDK kit's template for its kind.
//! The runtime holds no template of its own: the kit's names its pin type or node type with a
//! placeholder string, which the element's is written over.

use std::path::Path;

use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectNodeUiElement;

/// The kit's template for a control, under its `templates/`.
const CONTROL_TEMPLATE_FILE: &str = "control.ts";
/// The string a control template writes where its pin type goes.
const CONTROL_TEMPLATE_PIN_TYPE_PLACEHOLDER: &str = "\"PIN_TYPE_NAME\"";

/// The kit's template for a node body, under its `templates/`.
const NODE_BODY_TEMPLATE_FILE: &str = "node_body.ts";
/// The string a node body template writes where its node type goes.
const NODE_BODY_TEMPLATE_NODE_TYPE_PLACEHOLDER: &str = "\"NODE_TYPE_NAME\"";

/// A node UI element's script as rendered, and the kit it came from.
pub(crate) struct NodeUiElementTemplateRendered {
    /// The kit, with its version and where it was found.
    pub(crate) template_from: String,
    /// The script.
    pub(crate) content: String,
}

/// `element`'s script for the project at `project_directory`, rendered from the template of its
/// kind in the first kit found, or why it is not: no kit is found, it holds no template for the
/// kind, or the template names no placeholder for the type.
pub(crate) fn the_node_ui_element_template_rendered(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project_directory: &Path,
    element: &ProjectNodeUiElement,
) -> Result<NodeUiElementTemplateRendered, String> {
    let (template_file, placeholder, written_in) = match element {
        ProjectNodeUiElement::Control { pin_type, .. } => (
            CONTROL_TEMPLATE_FILE,
            CONTROL_TEMPLATE_PIN_TYPE_PLACEHOLDER,
            serde_json::to_string(pin_type).map_err(|why| why.to_string())?,
        ),
        ProjectNodeUiElement::NodeBody { node_type } => (
            NODE_BODY_TEMPLATE_FILE,
            NODE_BODY_TEMPLATE_NODE_TYPE_PLACEHOLDER,
            serde_json::to_string(node_type).map_err(|why| why.to_string())?,
        ),
    };
    let template_read = operations_on_the_loaded_streams
        .a_template_from_the_extension_sdk_kit(project_directory, template_file)?;
    if !template_read.template.contains(placeholder) {
        return Err(format!(
            "the {} template `templates/{template_file}` writes no {placeholder} for the type \
             to go in",
            template_read.kit
        ));
    }
    Ok(NodeUiElementTemplateRendered {
        template_from: template_read.kit,
        content: template_read.template.replace(placeholder, &written_in),
    })
}
