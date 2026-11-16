// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_engine::core::GeneratedNode;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

/// Declared in a nested module, because that is the part the crate name alone
/// cannot prove: the capture has to carry the author's module path, not just
/// the crate.
mod video_filters {
    use super::*;

    #[streamlib::sdk::node(execution = manual)]
    pub struct BlurNode;

    impl streamlib_engine::ManualNode for BlurNode::Node {
        fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
    }
}

#[streamlib::sdk::node(execution = manual)]
pub struct CrateRootNode;

impl streamlib_engine::ManualNode for CrateRootNode::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn import_path_of<P: GeneratedNode>() -> String {
    P::descriptor()
        .expect("the macro emits a descriptor for every node")
        .node_class_import_path
        .as_str()
        .to_string()
}

#[test]
fn a_node_in_a_nested_module_carries_crate_module_and_type() {
    assert_eq!(
        import_path_of::<video_filters::BlurNode::Node>(),
        "node_class_import_path_test::video_filters::BlurNode"
    );
}

#[test]
fn a_node_at_the_crate_root_carries_crate_and_type() {
    assert_eq!(
        import_path_of::<CrateRootNode::Node>(),
        "node_class_import_path_test::CrateRootNode"
    );
}
