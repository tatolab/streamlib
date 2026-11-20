// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The worked example of each operation the discovery tools list, set in one project: a stream
//! of a test pattern source, a clamp and an alpha-over compositor, a `Tremolo` function node an
//! agent writes, and a `knob` control for Float inputs.

mod assets_and_streams;
mod dependencies;
mod discovery_and_subscriptions;
mod graph_edits;
mod node_types;
mod playback_and_stream_actions;
mod project_files_and_views;
mod project_lifecycle;

pub(super) use assets_and_streams::*;
pub(super) use dependencies::*;
pub(super) use discovery_and_subscriptions::*;
pub(super) use graph_edits::*;
pub(super) use node_types::*;
pub(super) use playback_and_stream_actions::*;
pub(super) use project_files_and_views::*;
pub(super) use project_lifecycle::*;

/// The project's folder.
const PROJECT_DIRECTORY: &str = "/home/ada/Tatolab/night-show";
/// The project's UUID.
const PROJECT: &str = "5b0f3c1e-8d2a-4e6f-9a7b-1c2d3e4f5a6b";
/// The project's name.
const PROJECT_NAME: &str = "night-show";
/// The stream id `list_streams` answers for the project's one stream.
const STREAM: &str = "night-show-main-3fa2b1";
/// The UUID of that stream's asset.
const STREAM_ASSET: &str = "9e4d2b7a-3c1f-4a8e-b5d6-7f8091a2b3c4";
/// The test pattern source's node.
const SOURCE_NODE: &str = "2a7c9e41-6b3d-4f58-8e1a-0c9d7b5f3e21";
/// The clamp's node.
const CLAMP_NODE: &str = "c4e81b20-7d5a-4c93-a6f2-8b1e0d9c7a54";
/// The alpha-over compositor's node.
const COMPOSITOR_NODE: &str = "71f3a0d8-2e6c-4b95-9d47-a3c8e1f0b264";
/// The test pattern source's node type.
const SOURCE_NODE_TYPE: &str = "tatolab.testpattern:TestPatternSource";
/// The clamp's node type.
const CLAMP_NODE_TYPE: &str = "nodes.clamp:Clamp";
/// The alpha-over compositor's node type.
const COMPOSITOR_NODE_TYPE: &str = "tatolab.compositor.alpha_over_compositor:AlphaOverCompositor";
/// The node type of the function node an agent writes.
const TREMOLO_NODE_TYPE: &str = "nodes.tremolo:Tremolo";
/// A transaction the project's history records.
const TRANSACTION: &str = "e2b7d4c1-5a8f-4e3b-9c6d-0f1a2b3c4d5e";
/// The commit recording it.
const COMMIT: &str = "4f9c2e7a1b3d5f8e0a6c9b2d4e7f1a3c5b8d0e2f";
/// A stream action the runtime is running.
const STREAM_ACTION: &str = "3c8e1f5a-9b2d-4e7c-a0f6-1d4b7e9a2c5f";
/// A labelled group of nodes.
const GROUP: &str = "b5d2e8f1-4a7c-4b9e-8d3f-6a0c2e5b7d91";
/// A note on the stream's canvas.
const NOTE: &str = "d8a4c2e6-1f3b-4d5a-9c7e-0b2f4a6c8e13";
/// An imported project.
const IMPORTED_PROJECT: &str = "f1e2d3c4-b5a6-4978-8a9b-0c1d2e3f4a5b";
/// An asset of the project.
const ASSET: &str = "6a9c3e1f-7b5d-4f2a-8e0c-4d6b8a1c3e5f";

#[cfg(test)]
mod tests;
