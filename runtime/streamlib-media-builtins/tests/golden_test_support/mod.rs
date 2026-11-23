// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The nodes both golden tests take out of a golden before it loads on a runtime with no
//! interpreter.

use streamlib::sdk::descriptors::NodeClassImportPath;

/// Each built-in node type a golden places that this runtime does not register, with no typed
/// form a rename reads it as.
fn built_in_node_types_this_runtime_does_not_register() -> [NodeClassImportPath; 2] {
    ["ChannelsToRecords", "RecordsToChannels"].map(NodeClassImportPath::of_built_in_node)
}

/// Whether a node of `node_type` is taken out of a golden before it loads: a module's node, which
/// a runtime with no interpreter cannot describe, or a node of a built-in type this runtime does
/// not register.
pub(crate) fn is_taken_out_before_a_load(node_type: &NodeClassImportPath) -> bool {
    !node_type.names_a_built_in_node()
        || built_in_node_types_this_runtime_does_not_register().contains(node_type)
}
