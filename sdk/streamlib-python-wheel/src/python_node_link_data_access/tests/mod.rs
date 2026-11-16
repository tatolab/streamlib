// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use std::sync::Arc;

use pyo3::exceptions::PyTypeError;
use pyo3::types::{PyBytes, PyDict, PyString};
use streamlib::sdk::error::Error;
use streamlib::sdk::iceoryx2::{
    ChannelsBlockSampleType, DataChannelPayloadDescriptor, Iceoryx2Node, MessageGroupBoundary,
    PacketsUnitDescriptorBody,
};
#[cfg(target_os = "linux")]
use streamlib::sdk::iceoryx2::{
    GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody, ImageFrameH273Colour,
};
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::TextureFormat;

use super::*;
use crate::python_bag_conversion::records_message_payload_of;
use crate::python_node_context::PythonGpuContextLimitedAccess;
use crate::python_port_family_read_types::{
    PythonArrayFrameCpuAttribute, PythonChannelsBlock, PythonPacketsUnit,
};
use crate::python_port_payload::PythonPortPayloadBorrow;

mod borrows_and_loans;
mod family_round_trips;
mod helper_wiring_boards_and_losses;
mod image_and_array_frames;

/// Unique per test: every test in this process shares one iceoryx2 domain,
/// so a fixed name would let one test's channel meet the next one's.
fn unique_channel_names(label: &str) -> (String, String) {
    let run = std::process::id();
    (
        format!("wiring{run}_{label}/frames_to_downstream"),
        format!("wiring{run}_{label}_dest/notify"),
    )
}

fn helper_plane() -> PythonNodeLinkDataAccess {
    PythonNodeLinkDataAccess::over_helper_process_iceoryx2_node(
        Iceoryx2Node::for_this_test_process(),
    )
}

/// A source and a destination plane joined by one link, `frames_to_downstream`
/// into `frames_from_upstream`.
///
/// The destination subscribes first: iceoryx2 drops a send with no
/// subscriber attached, so wiring the publisher first would race.
fn one_wired_link(
    python: Python<'_>,
    label: &str,
) -> (PythonNodeLinkDataAccess, PythonNodeLinkDataAccess) {
    let (channel, notify) = unique_channel_names(label);
    let source = helper_plane();
    let destination = helper_plane();
    destination
        .wire_input_link(
            python,
            "frames_from_upstream",
            &channel,
            &channel,
            &notify,
            "read_next_in_order",
            8,
            8,
            2,
            1,
            "link-1",
            None,
            None,
            None,
            None,
        )
        .unwrap();
    source
        .wire_output_link(
            python,
            "frames_to_downstream",
            &channel,
            &notify,
            1024,
            1 << 20,
            8,
            2,
            1,
            "link-1",
            None,
        )
        .unwrap();
    (source, destination)
}

/// The next message on the one-link fixture's input, read as its family's type.
fn read_as_the_family_type<'py>(
    python: Python<'py>,
    destination: &PythonNodeLinkDataAccess,
    family: PortFamilyOfTheRead,
) -> Bound<'py, PyAny> {
    destination
        .read_from_input_port_offering_gpu_access(
            python,
            "frames_from_upstream",
            PortPayloadReadShape::FamilyType(PortFamiliesOfTheRead::of_one(family)),
            None,
        )
        .unwrap()
        .expect("the wired input received nothing")
}

/// The stream distribution's source root on `sys.path`, where the family
/// types a read builds live; a `cargo test` run has no installed
/// `tatolab-stream`.
fn put_the_stream_distribution_source_on_sys_path(python: Python<'_>) {
    let stream_distribution_source_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../tatolab-stream");
    let sys_path = python.import("sys").unwrap().getattr("path").unwrap();
    if !sys_path.contains(stream_distribution_source_root).unwrap() {
        sys_path
            .call_method1("insert", (0, stream_distribution_source_root))
            .unwrap();
    }
}

/// Lend this interpreter the one runtime callable a family type's claim
/// reaches, as the runtime lends `tatolab.runtime._engine` to a node's.
#[cfg(target_os = "linux")]
fn lend_the_typed_read_gpu_offer_to_this_interpreter(python: Python<'_>) {
    let lent_engine = pyo3::types::PyModule::new(python, "_engine").unwrap();
    lent_engine
        .add_function(
            pyo3::wrap_pyfunction!(
                crate::python_bag_conversion::gpu_limited_access_of_the_typed_read_in_progress,
                &lent_engine
            )
            .unwrap(),
        )
        .unwrap();
    let sys_modules = python.import("sys").unwrap().getattr("modules").unwrap();
    sys_modules
        .set_item(
            "tatolab.runtime",
            pyo3::types::PyModule::new(python, "tatolab.runtime").unwrap(),
        )
        .unwrap();
    sys_modules
        .set_item("tatolab.runtime._engine", lent_engine)
        .unwrap();
}

/// Take back what [`lend_the_typed_read_gpu_offer_to_this_interpreter`]
/// lent, so later tests in this interpreter see no runtime lent.
#[cfg(target_os = "linux")]
fn withdraw_the_lent_typed_read_gpu_offer(python: Python<'_>) {
    python
        .run(
            c"import sys\n\
              sys.modules.pop('tatolab.runtime._engine', None)\n\
              sys.modules.pop('tatolab.runtime', None)\n\
              import tatolab.stream._runtime_lend as lend\n\
              lend._native_callables_resolved_by_name.clear()\n",
            None,
            None,
        )
        .unwrap();
}

/// A source and a destination plane joined by one link, as the Python
/// objects a helper's code holds.
fn one_wired_link_as_python_objects<'py>(python: Python<'py>, label: &str) -> Bound<'py, PyDict> {
    let (source, destination) = one_wired_link(python, label);
    let planes = PyDict::new(python);
    planes
        .set_item("source", Bound::new(python, source).unwrap())
        .unwrap();
    planes
        .set_item("destination", Bound::new(python, destination).unwrap())
        .unwrap();
    planes
}
