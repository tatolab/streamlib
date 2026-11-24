// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The settings the config schema of a node type this stream resolves lists, and whether the
//! stream describes a type at all.

use super::NodeTypesOneStreamResolves;
use crate::core::descriptors::NodeClassImportPath;

impl NodeTypesOneStreamResolves {
    /// The name of each setting the config schema of `node_type` lists, which a node of it holds
    /// among its input values though no input port of the type carries it; none for a type with
    /// no schema.
    pub(crate) fn the_settings_its_config_schema_lists(
        &self,
        node_type: &NodeClassImportPath,
    ) -> std::collections::HashSet<String> {
        crate::core::nodes::the_settings_a_config_schema_lists(
            self.descriptor(node_type)
                .and_then(|descriptor| descriptor.config_schema)
                .as_ref(),
        )
    }

    /// Whether the config schema of `node_type` lists the setting `name`.
    pub(crate) fn its_config_schema_lists_the_setting(
        &self,
        node_type: &NodeClassImportPath,
        name: &str,
    ) -> bool {
        self.descriptor(node_type)
            .and_then(|descriptor| descriptor.config_schema)
            .is_some_and(|config_schema| {
                config_schema
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|properties| properties.contains_key(name))
            })
    }

    /// Whether this stream describes `node_type`: its node code described it and the stream
    /// resolves its ports.
    pub(crate) fn describes(&self, node_type: &NodeClassImportPath) -> bool {
        self.placeholder_of_the_node_type_not_described(node_type)
            .is_none()
            && self.port_info(node_type).is_some()
    }
}
