// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The read and the write of a bundle holding a Texture, which travels as one Bundle frame: its
//! small members in the frame's Records row, and each Texture by the pool slot it lives in, read
//! on a wire of its own with its slot claimed.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use serde_json::Value;
use streamlib::sdk::descriptors::PinType;
use streamlib::sdk::iceoryx2::{
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody,
};
use streamlib::sdk::pin_value_packing::{
    HeavyMemberOfABundleFrame, PackedPinValue, PackedPinValueRef,
    the_bundle_a_bundle_frame_carries, the_bundle_frame_of_a_bundle,
};

use super::{PythonLinkInputDataReader, PythonLinkOutputDataWriter};
use crate::python_node_link_data_access::{
    image_frame_descriptor_of_the_image, image_frame_of_the_read,
};

fn the_bundle_given_as_json(pin_type: &str, what_it_is: &str) -> PyResult<PinType> {
    let pin_type: PinType = serde_json::from_str(pin_type).map_err(|refusal| {
        PyValueError::new_err(format!(
            "{what_it_is}'s pin type {pin_type} does not read: {refusal}"
        ))
    })?;
    if !pin_type.holds_a_heavy_member() {
        return Err(PyValueError::new_err(format!(
            "{what_it_is} is a {pin_type}, which holds no Texture, so it travels as the one \
             message its members ride"
        )));
    }
    Ok(pin_type)
}

/// Each member of the bundle holding a Texture `input_port` holds for this read, a value of
/// `pin_type`, given as JSON, by name: each small member its stored JSON form, each one the
/// frame leaves out its default, and each Texture member a `Texture`, its pool slot claimed;
/// `None` when no message arrived since the last read. Any other message, and a frame missing a
/// member the bundle declares, is refused by name.
#[pyfunction]
#[pyo3(signature = (*, inputs, node_id, input_port, pin_type))]
pub(crate) fn read_the_bundle_frame_at_an_input_port<'py>(
    python: Python<'py>,
    inputs: &PythonLinkInputDataReader,
    node_id: &str,
    input_port: &str,
    pin_type: &str,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let what_it_is = format!("`{node_id}`'s input `{input_port}`");
    let bundle = the_bundle_given_as_json(pin_type, &what_it_is)?;
    let Some(message) = inputs
        .link_data_access
        .get()
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
    else {
        return Ok(None);
    };
    let read = the_bundle_a_bundle_frame_carries(
        &bundle,
        PackedPinValueRef {
            payload_descriptor: &message.payload_descriptor,
            payload: message.payload.bytes(),
        },
    )
    .map_err(|refusal| {
        PyValueError::new_err(format!("{what_it_is} is a {bundle}, and {refusal}"))
    })?;
    let members = python
        .import("json")?
        .call_method1(
            "loads",
            (serde_json::to_string(&Value::Object(read.small_members))
                .expect("a JSON map encodes"),),
        )?
        .cast_into::<PyDict>()?;
    let gpu_limited_access = inputs.gpu_limited_access_context.bind(python);
    for heavy_member in &read.heavy_members {
        if heavy_member.descriptor.known_kind() != Ok(DataChannelPayloadDescriptorKind::ImageFrame)
        {
            return Err(PyValueError::new_err(format!(
                "{what_it_is}'s member `{}` is read in a bundle only as a Texture",
                heavy_member.name
            )));
        }
        let image = image_frame_of_the_read(
            python,
            input_port,
            ImageFrameDescriptorBody::from_descriptor_body(&heavy_member.descriptor.body),
            message.timestamp_ns,
            Some(gpu_limited_access),
        )?;
        members.set_item(&heavy_member.name, image)?;
    }
    Ok(Some(members))
}

/// Write a bundle holding a Texture, a value of `pin_type`, given as JSON, on `output_port` as
/// one Bundle frame: `small_members`, the stored JSON form of its small members by name, each
/// one it leaves out given its default, and `textures`, each Texture member by name, an
/// `Texture` this node read or a texture it acquired from a pool, carried by its pool slot.
#[pyfunction]
#[pyo3(signature = (*, outputs, output_port, pin_type, small_members, textures, timestamp_ns = None))]
pub(crate) fn write_the_bundle_frame_on_an_output_port(
    python: Python<'_>,
    outputs: &PythonLinkOutputDataWriter,
    output_port: &str,
    pin_type: &str,
    small_members: &str,
    textures: &Bound<'_, PyDict>,
    timestamp_ns: Option<i64>,
) -> PyResult<()> {
    let what_it_is = format!("output `{output_port}`");
    let bundle = the_bundle_given_as_json(pin_type, &what_it_is)?;
    let small_members: Value = serde_json::from_str(small_members).map_err(|refusal| {
        PyValueError::new_err(format!(
            "{what_it_is}'s small members {small_members} do not read: {refusal}"
        ))
    })?;
    let gpu_limited_access = outputs.gpu_limited_access_context.get();
    let texture_members = textures
        .iter()
        .map(|(name, image)| {
            let name: String = name.extract()?;
            let image_frame =
                image_frame_descriptor_of_the_image(output_port, &image, &[], gpu_limited_access)?;
            Ok((name, DataChannelPayloadDescriptor::image_frame(image_frame)))
        })
        .collect::<PyResult<Vec<_>>>()?;
    let heavy_members: Vec<HeavyMemberOfABundleFrame<'_>> = texture_members
        .iter()
        .map(|(name, descriptor)| HeavyMemberOfABundleFrame {
            name,
            descriptor: *descriptor,
            payload: &[],
        })
        .collect();
    let PackedPinValue {
        payload_descriptor,
        payload,
    } = the_bundle_frame_of_a_bundle(&bundle, &small_members, &heavy_members).map_err(
        |refusal| PyValueError::new_err(format!("{what_it_is} is a {bundle}, and {refusal}")),
    )?;
    outputs.link_data_access.get().write_described_by(
        python,
        output_port,
        PyBytes::new(python, &payload).as_any(),
        timestamp_ns,
        true,
        |_| Ok(payload_descriptor),
    )
}
