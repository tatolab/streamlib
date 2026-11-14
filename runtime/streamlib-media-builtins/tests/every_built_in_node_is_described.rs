// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The description lint, turned on for the runtime's own built-in nodes.

use streamlib::sdk::descriptors::lint_the_descriptions_of_a_node_type;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;

#[test]
fn every_built_in_node_describes_itself_its_ports_and_its_config_keys() {
    streamlib_media_builtins::register_media_builtin_processor_types();
    let built_in_node_descriptors: Vec<_> = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .filter(|descriptor| {
            descriptor
                .processor_class_import_path
                .names_a_built_in_node()
        })
        .collect();
    assert!(
        !built_in_node_descriptors.is_empty(),
        "this floor registers built-in nodes"
    );

    let findings: Vec<String> = built_in_node_descriptors
        .iter()
        .flat_map(|descriptor| {
            lint_the_descriptions_of_a_node_type(descriptor)
                .into_iter()
                .map(move |finding| {
                    format!("{}: {finding}", descriptor.processor_class_import_path)
                })
        })
        .collect();
    assert!(findings.is_empty(), "{findings:#?}");
}
