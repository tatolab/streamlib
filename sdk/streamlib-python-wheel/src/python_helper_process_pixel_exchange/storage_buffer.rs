// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Acquiring a tensor storage buffer from the parent, on either floor: the
//! escalate op, then a checkout whose platform arm imports the memory.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib::sdk::rhi::TensorStorageBufferLayout;

use super::{
    HelperCheckedOutStorageBuffer, HelperCheckedOutSurface, HelperProcessGpuExchangeClient,
    ProcessorOutputPoolRequest, escalate_round_trip_to_parent, response_field,
};

impl HelperProcessGpuExchangeClient {
    /// Ask the parent for a tensor storage buffer — a one-off this helper owes
    /// a release, or the next tensor of a processor output pool — then check
    /// it out.
    pub(crate) fn acquire_storage_buffer(
        self: &Arc<Self>,
        python: Python<'_>,
        tensor_layout: &TensorStorageBufferLayout,
        processor_output_pool: Option<ProcessorOutputPoolRequest<'_>>,
    ) -> PyResult<HelperCheckedOutStorageBuffer> {
        let op = PyDict::new(python);
        op.set_item("op", "acquire_storage_buffer")?;
        op.set_item("shape", tensor_layout.shape())?;
        op.set_item("dtype", tensor_layout.element_type().wire_name())?;
        ProcessorOutputPoolRequest::write_onto_escalate_op(processor_output_pool, python, &op)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let surface_id: String = response_field(&response, "handle_id")?.extract()?;
        // Bound before the checkout, so a failed checkout or a malformed
        // answer still pays the release instead of stranding the allocation.
        let release_to_parent =
            self.release_debt_unless_pooled(python, processor_output_pool, &surface_id);
        let checked_out = python.detach(|| self.check_out_and_import(&surface_id))?;
        let HelperCheckedOutSurface::StorageBuffer(mut checked_out_storage_buffer) = checked_out
        else {
            return Err(PyRuntimeError::new_err(format!(
                "acquire_storage_buffer's allocation {surface_id:?} checked out as another \
                 resource type; the parent registers a tensor as a storage_buffer"
            )));
        };
        if checked_out_storage_buffer.tensor_layout != *tensor_layout {
            return Err(PyRuntimeError::new_err(format!(
                "acquire_storage_buffer asked for {tensor_layout} and the parent registered {}",
                checked_out_storage_buffer.tensor_layout
            )));
        }
        checked_out_storage_buffer.writable = true;
        checked_out_storage_buffer.release_to_parent = release_to_parent;
        Ok(checked_out_storage_buffer)
    }
}

/// The declared tensor layout a storage-buffer checkout carries, refusing a
/// missing or malformed `shape` or `dtype` by name.
pub(super) fn tensor_layout_of_a_check_out(
    surface_id: &str,
    response: &serde_json::Value,
) -> PyResult<TensorStorageBufferLayout> {
    TensorStorageBufferLayout::from_surface_share_fields(response).map_err(|refusal| {
        PyRuntimeError::new_err(format!("tensor surface {surface_id:?}: {refusal}"))
    })
}

#[cfg(test)]
mod tests {
    use super::tensor_layout_of_a_check_out;

    #[test]
    fn a_check_out_names_its_tensor_layout() {
        let layout = tensor_layout_of_a_check_out(
            "tensor-a",
            &serde_json::json!({"shape": [1, 3, 640, 640], "dtype": "float32"}),
        )
        .expect("a well-formed checkout parses");
        assert_eq!(layout.shape(), &[1, 3, 640, 640]);
        assert_eq!(layout.byte_size(), 3 * 640 * 640 * 4);
    }

    #[test]
    fn a_check_out_with_a_malformed_shape_is_refused_by_name() {
        pyo3::Python::initialize();
        let refusal = tensor_layout_of_a_check_out(
            "tensor-b",
            &serde_json::json!({"shape": [3, -1], "dtype": "float32"}),
        )
        .expect_err("a negative dimension is not a shape");
        assert!(refusal.to_string().contains("no shape"));
    }
}
