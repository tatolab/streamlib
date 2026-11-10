// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `tatolab.studio:StudioView` built-in: a node whose ports are a Tato Studio view's declared
//! inputs and outputs, whose outputs publish the values a panel writes onto them.

mod studio_view_node_type;
mod studio_view_output_payload;
mod studio_view_processor;

pub(crate) use studio_view_node_type::register_the_studio_view_node_type;
pub use studio_view_node_type::{
    STUDIO_VIEW_NODE_TYPE, StudioViewNodeConfig, is_the_studio_view_node_type,
    the_studio_view_node_type,
};
pub(crate) use studio_view_processor::StudioViewProcessor;

#[cfg(test)]
#[path = "studio_view_tests.rs"]
mod studio_view_tests;
