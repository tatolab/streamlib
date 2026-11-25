// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures shared by the graph snapshot's tests.

use crate::core::descriptors::{PinDeclaration, PinType};
use super::*;
use std::collections::HashSet;

use uuid::Uuid;

use crate::core::Error;
use crate::core::context::StreamClockSpec;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::json_schema::LinkPortRefOutput;
use crate::core::nodes::NODE_REGISTRY;
use crate::core::nodes::NodeTypesOneStreamResolves;

mod graph_document_reading_and_writing_tests;
mod references_interfaces_variables_and_groups_tests;

const A_CAMERA_CLASS: &str = "my_app.nodes:CameraNode";

/// A camera class registered descriptor-only, with an `frames_in` input and
/// a `video` output. Idempotent across tests.
fn a_registered_camera_class() -> &'static str {
    const REGISTERED_CAMERA_CLASS: &str = "graph_snapshot_tests:RegisteredCamera";
    let _ = NODE_REGISTRY.register_descriptor_only(
        crate::core::descriptors::NodeDescriptor::new(
            crate::core::descriptors::NodeClassShortName::new("RegisteredCamera").unwrap(),
            NodeClassImportPath::new(REGISTERED_CAMERA_CLASS).unwrap(),
            "graph snapshot test",
        )
        .with_input(crate::core::descriptors::PortDescriptor::new("frames_in", "", false, PinDeclaration::exactly(PinType::Texture { pixel_formats: Vec::new() })))
        .with_output(crate::core::descriptors::PortDescriptor::new("video", "", false, PinDeclaration::exactly(PinType::Texture { pixel_formats: Vec::new() }))),
    );
    REGISTERED_CAMERA_CLASS
}
