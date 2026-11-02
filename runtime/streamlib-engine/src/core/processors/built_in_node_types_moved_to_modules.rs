// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The built-in node types that left the runtime for a first-party module, so
//! a recorded graph still naming one is refused naming the module to install.

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};

/// One former built-in, by its class name in `tatolab.stream`, with the
/// distribution that ships it now and the node type it is named by there.
struct BuiltInNodeTypeMovedToAModule {
    built_in_class_name: &'static str,
    module_distribution: &'static str,
    module_node_type: &'static str,
}

const BUILT_IN_NODE_TYPES_MOVED_TO_MODULES: &[BuiltInNodeTypeMovedToAModule] = &[
    BuiltInNodeTypeMovedToAModule {
        built_in_class_name: "OpusEncoder",
        module_distribution: "tatolab-opus",
        module_node_type: "tatolab.opus:OpusEncoder",
    },
    BuiltInNodeTypeMovedToAModule {
        built_in_class_name: "OpusDecoder",
        module_distribution: "tatolab-opus",
        module_node_type: "tatolab.opus:OpusDecoder",
    },
];

/// Refuse a former built-in's type, naming the module that ships it now and
/// the type to name instead.
pub(crate) fn refuse_a_built_in_node_type_moved_to_a_module(
    node_type: &ProcessorClassImportPath,
) -> Result<()> {
    if !node_type.names_a_built_in_node() {
        return Ok(());
    }
    match BUILT_IN_NODE_TYPES_MOVED_TO_MODULES.iter().find(|moved| {
        *node_type == ProcessorClassImportPath::of_built_in_node(moved.built_in_class_name)
    }) {
        Some(moved) => Err(Error::BuiltInNodeTypeMovedToAModule {
            node_type: node_type.clone(),
            module_distribution: moved.module_distribution.to_string(),
            module_node_type: moved.module_node_type.to_string(),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_former_built_in_is_refused_naming_the_module_to_install_and_the_type_to_name() {
        for (class_name, module_node_type) in [
            ("OpusEncoder", "tatolab.opus:OpusEncoder"),
            ("OpusDecoder", "tatolab.opus:OpusDecoder"),
        ] {
            let refusal = refuse_a_built_in_node_type_moved_to_a_module(
                &ProcessorClassImportPath::of_built_in_node(class_name),
            )
            .expect_err("a former built-in is refused")
            .to_string();

            assert!(
                refusal.contains(&format!("`tatolab.stream:{class_name}`")),
                "{refusal}"
            );
            assert!(refusal.contains("`tatolab-opus`"), "{refusal}");
            assert!(
                refusal.contains(&format!("`{module_node_type}`")),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_built_in_still_in_the_runtime_and_a_module_type_are_not_refused() {
        for still_named in [
            ProcessorClassImportPath::of_built_in_node("SpeakerSink"),
            ProcessorClassImportPath::new("tatolab.opus:OpusEncoder").expect("a type"),
        ] {
            assert!(
                refuse_a_built_in_node_type_moved_to_a_module(&still_named).is_ok(),
                "{still_named}"
            );
        }
    }

    #[test]
    fn every_module_type_the_table_names_is_the_former_built_ins_own_class_name() {
        for moved in BUILT_IN_NODE_TYPES_MOVED_TO_MODULES {
            let module_node_type =
                ProcessorClassImportPath::new(moved.module_node_type).expect("a type");
            assert!(
                !module_node_type.names_a_built_in_node(),
                "{module_node_type}"
            );
            assert_eq!(
                moved
                    .module_node_type
                    .rsplit_once(':')
                    .map(|(_, class)| class),
                Some(moved.built_in_class_name),
                "a node keeps its class name when it moves"
            );
        }
    }
}
