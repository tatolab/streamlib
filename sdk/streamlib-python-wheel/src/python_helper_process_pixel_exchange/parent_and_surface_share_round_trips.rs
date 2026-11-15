// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The child's round trips to its parent and to the surface-share service:
//! escalate ops, checkouts and their releases, layout publishes, and the
//! import device every checkout maps through.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib_consumer_rhi::ConsumerVulkanDevice;
use streamlib_gpu_frame_import_client::SurfaceShareTransferredHandle;

use super::{
    HelperCheckedOutPixelSurface, HelperCheckedOutSurface, HelperProcessGpuExchangeClient,
    HelperSurfaceCheckOutLeaseDebt, HelperSurfaceHeldAgainstProducerReuse,
    HelperSurfaceReleaseDebt, ProcessorOutputPoolRequest,
};

/// One field of an escalate response, named in the failure so a parent
/// that answered a shape this child does not understand says which part.
pub(super) fn response_field<'py>(
    response: &Bound<'py, PyAny>,
    field: &str,
) -> PyResult<Bound<'py, PyAny>> {
    response.get_item(field).map_err(|_| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's response carried no {field}"
        ))
    })
}

/// One escalate round trip to the parent, called with the GIL attached.
///
/// The callable is the bridge's `request_from_parent`, whose wait on the
/// response releases the GIL — a slow parent parks this thread, never the
/// interpreter's others.
pub(super) fn escalate_round_trip_to_parent<'py>(
    python: Python<'py>,
    escalate_request_to_parent: &Py<PyAny>,
    op: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    escalate_request_to_parent
        .bind(python)
        .call1((op,))
        .map_err(|request_failure| {
            crate::python_processor_context::gpu_operation_error(format!(
                "the parent refused or failed the GPU request: {request_failure}"
            ))
        })
}

impl ProcessorOutputPoolRequest<'_> {
    /// Name the pool on an acquire's escalate op; a one-off acquire carries
    /// no `processor_output_pool` field.
    pub(super) fn write_onto_escalate_op(
        processor_output_pool: Option<Self>,
        python: Python<'_>,
        op: &Bound<'_, PyDict>,
    ) -> PyResult<()> {
        let Some(processor_output_pool) = processor_output_pool else {
            return Ok(());
        };
        let field = PyDict::new(python);
        field.set_item("pool_key", processor_output_pool.pool_key)?;
        field.set_item("rotation_depth", processor_output_pool.rotation_depth)?;
        op.set_item("processor_output_pool", field)
    }
}

/// Hand the parent's `release_handle` for `handle_id` to the bridge's release
/// worker and return without waiting on its answer.
///
/// The callable is the bridge's `release_to_parent_without_waiting`. Every
/// release a drop owes goes this way: the drop can be a garbage-collector
/// finalizer on the bridge's reader, which a round trip would stall for the
/// whole escalate timeout, since only that thread delivers the answer.
pub(super) fn hand_a_release_handle_to_the_release_worker(
    python: Python<'_>,
    release_to_parent_without_waiting: &Py<PyAny>,
    handle_id: &str,
) -> PyResult<()> {
    let release_op = PyDict::new(python);
    release_op.set_item("op", "release_handle")?;
    release_op.set_item("handle_id", handle_id)?;
    release_to_parent_without_waiting
        .bind(python)
        .call1((release_op,))?;
    Ok(())
}

/// One `u32` field of a checkout's registration metadata, present and
/// positive or refused naming the field.
pub(super) fn required_positive_u32_check_out_metadata_field(
    response: &serde_json::Value,
    surface_id: &str,
    field: &str,
) -> PyResult<u32> {
    response
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} carried no usable {field}"
            ))
        })
}

/// Raise the service's own refusal of a checkout, if it refused one.
///
/// `checked_out_subject` names what was being checked out, because the caller
/// knows whether it asked for a published surface or the staging behind one and
/// the response does not. Taken as `format_args!` so the happy path — every
/// frame a consumer claims or resolves — formats nothing.
pub(super) fn refuse_check_out_the_service_declined(
    checked_out_subject: std::fmt::Arguments<'_>,
    response: &serde_json::Value,
) -> PyResult<()> {
    match response.get("error").and_then(|value| value.as_str()) {
        Some(checkout_error) => Err(PyRuntimeError::new_err(format!(
            "the surface-share service refused check_out of {checked_out_subject}: \
             {checkout_error}"
        ))),
        None => Ok(()),
    }
}

/// A surface-share answer and the handles it carried.
pub(super) type SurfaceShareAnswer = (serde_json::Value, Vec<SurfaceShareTransferredHandle>);

impl HelperProcessGpuExchangeClient {
    /// The release an acquire owes its parent from the moment the parent
    /// allocated: a one-off's own `release_handle`; a processor output pool's
    /// frame owes none, since the pool owns the slot.
    pub(super) fn release_debt_unless_pooled(
        &self,
        python: Python<'_>,
        processor_output_pool: Option<ProcessorOutputPoolRequest<'_>>,
        handle_id: &str,
    ) -> Option<HelperSurfaceReleaseDebt> {
        processor_output_pool
            .is_none()
            .then(|| HelperSurfaceReleaseDebt {
                release_to_parent_without_waiting: self
                    .release_to_parent_without_waiting
                    .clone_ref(python),
                handle_id: handle_id.to_owned(),
            })
    }

    /// Ask the parent to allocate, then check the result out and import it.
    ///
    /// Called attached; the escalate wait releases the GIL, and the checkout
    /// and Vulkan import run detached.
    pub(crate) fn acquire_pixel_buffer(
        self: &Arc<Self>,
        python: Python<'_>,
        width: u32,
        height: u32,
        wire_format_name: &str,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        let op = PyDict::new(python);
        op.set_item("op", "acquire_pixel_buffer")?;
        op.set_item("width", width)?;
        op.set_item("height", height)?;
        op.set_item("format", wire_format_name)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let handle_id: String = response
            .get_item("handle_id")
            .map_err(|_| {
                PyRuntimeError::new_err(
                    "the parent's acquire_pixel_buffer response carried no handle_id",
                )
            })?
            .extract()?;
        // The debt exists from the moment the parent allocated: if the
        // checkout or the Vulkan import below fails, this drops on the error
        // path and pays the `release_handle`, instead of stranding the
        // parent's pool slot and surface-share entry until teardown.
        let release_to_parent = HelperSurfaceReleaseDebt {
            release_to_parent_without_waiting: self
                .release_to_parent_without_waiting
                .clone_ref(python),
            handle_id: handle_id.clone(),
        };
        let checked_out = python.detach(|| self.check_out_and_import(&handle_id))?;
        let HelperCheckedOutSurface::PixelBuffer(mut checked_out_pixel_surface) = checked_out
        else {
            return Err(PyRuntimeError::new_err(format!(
                "acquire_pixel_buffer's allocation {handle_id:?} resolved to a texture \
                 registration; a pool cannot answer a buffer acquire with an image"
            )));
        };
        checked_out_pixel_surface.release_to_parent = Some(release_to_parent);
        Ok(checked_out_pixel_surface)
    }

    /// Check out a surface another processor published — pixel buffer or
    /// texture, whichever its registration names. No release debt: the
    /// surface belongs to its acquirer.
    pub(crate) fn resolve_surface(
        self: &Arc<Self>,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<HelperCheckedOutSurface> {
        python.detach(|| self.check_out_and_import(surface_id))
    }

    /// Claim a published surface against producer reuse, without importing
    /// its memory.
    ///
    /// The cheap half of [`Self::resolve_surface`]: the checkout is what mints
    /// the lease, and a holder that only needs the frame to hold still owes no
    /// Vulkan import for it. A pooled frame its pool's claim table covers is
    /// claimed in that table with no request; any other is checked out, and the
    /// plane fds the service delivers alongside the claim close with this call.
    pub(crate) fn claim_surface_against_producer_reuse(
        self: &Arc<Self>,
        surface_id: &str,
    ) -> PyResult<HelperSurfaceHeldAgainstProducerReuse> {
        if let Some(claimed) = self.claim_in_its_pool_claim_table(surface_id)? {
            return Ok(HelperSurfaceHeldAgainstProducerReuse::GpuPoolClaim(claimed));
        }
        let (response, _transferred_handles_released_by_scope) =
            self.check_out_surface(surface_id)?;
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), &response)?;
        Ok(HelperSurfaceHeldAgainstProducerReuse::CheckOutLease(
            HelperSurfaceCheckOutLeaseDebt {
                exchange_client: Arc::clone(self),
                surface_id: surface_id.to_string(),
            },
        ))
    }

    /// Wait for the parent's GPU device to go idle.
    ///
    /// A real wait, not an acknowledgement: the parent runs it inside its
    /// escalate scope, so the reply means the device was idle on that
    /// side — which is the only side there is.
    pub(crate) fn wait_device_idle(&self, python: Python<'_>) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "wait_device_idle")?;
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Claim a surface against producer reuse and take the handles its
    /// memory crosses as.
    ///
    /// The one place this op is spelled: a checkout is what pins the frame,
    /// and every caller owes the matching [`Self::release_check_out`].
    pub(super) fn check_out_surface(&self, surface_id: &str) -> PyResult<SurfaceShareAnswer> {
        self.surface_share_request(&serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        }))
    }

    /// Let go of one claim on a surface, freeing its slot for its producer.
    pub(super) fn release_check_out(&self, surface_id: &str) -> PyResult<SurfaceShareAnswer> {
        self.surface_share_request(&serde_json::json!({
            "op": "release_check_out",
            "surface_id": surface_id,
        }))
    }

    /// Publish the layout this side left a texture in, so the next
    /// consumer's acquire barrier names the right source layout.
    pub(super) fn publish_image_layout_to_surface_share(
        &self,
        surface_id: &str,
        current_image_layout_raw: i32,
    ) -> PyResult<()> {
        let (response, _no_handles) = self.surface_share_request(&serde_json::json!({
            "op": "update_layout",
            "surface_id": surface_id,
            "current_image_layout": current_image_layout_raw,
        }))?;
        if let Some(publish_error) = response.get("error").and_then(|value| value.as_str()) {
            return Err(PyRuntimeError::new_err(format!(
                "the surface-share service refused the layout publish for {surface_id:?}: \
                 {publish_error}"
            )));
        }
        match response.get("success").and_then(|value| value.as_bool()) {
            Some(true) => Ok(()),
            Some(false) => Err(PyRuntimeError::new_err(format!(
                "the surface-share service did not record the layout publish for \
                 {surface_id:?} — it knows no such registration"
            ))),
            None => Err(PyRuntimeError::new_err(format!(
                "the surface-share service's layout-publish answer for {surface_id:?} \
                 carried no success field"
            ))),
        }
    }

    pub(super) fn consumer_vulkan_device(&self) -> PyResult<Arc<ConsumerVulkanDevice>> {
        let mut device = self.consumer_vulkan_device.lock();
        if let Some(existing_device) = device.as_ref() {
            return Ok(Arc::clone(existing_device));
        }
        let could_not_create_the_import_device = |device_failure: &dyn std::fmt::Display| {
            PyRuntimeError::new_err(format!(
                "this helper process could not create its Vulkan import device: {device_failure}"
            ))
        };
        let vulkan_device_uuid_the_runtime_named =
            match &self.vulkan_device_uuid_the_runtime_named_at_spawn {
                Ok(vulkan_device_uuid_the_runtime_named) => *vulkan_device_uuid_the_runtime_named,
                Err(absent_or_malformed) => {
                    return Err(could_not_create_the_import_device(absent_or_malformed));
                }
            };
        let created = Arc::new(
            ConsumerVulkanDevice::new_on_the_device_the_runtime_named(
                vulkan_device_uuid_the_runtime_named,
            )
            .map_err(|device_failure| could_not_create_the_import_device(&device_failure))?,
        );
        *device = Some(Arc::clone(&created));
        Ok(created)
    }
}
