// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Prints every built-in this floor registers as a JSON array of catalog
//! entries, sorted by `type` — the input `cargo xtask
//! generate-built-in-node-classes` renders `tatolab.stream`'s built-in classes
//! from.

use std::io::Write;

use streamlib::sdk::json_schema::ProcessorDescriptorOutput;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;

fn main() -> std::io::Result<()> {
    streamlib_media_builtins::register_media_builtin_processor_types();
    let mut built_in_node_descriptors: Vec<ProcessorDescriptorOutput> = PROCESSOR_REGISTRY
        .list_registered()
        .iter()
        .filter(|descriptor| {
            descriptor
                .processor_class_import_path
                .names_a_built_in_node()
        })
        .map(ProcessorDescriptorOutput::from)
        .collect();
    built_in_node_descriptors.sort_by(|left, right| {
        left.processor_class_import_path
            .as_str()
            .cmp(right.processor_class_import_path.as_str())
    });
    let rendered = serde_json::to_string_pretty(&built_in_node_descriptors)?;
    let mut standard_output = std::io::stdout().lock();
    standard_output.write_all(rendered.as_bytes())?;
    standard_output.write_all(b"\n")
}
