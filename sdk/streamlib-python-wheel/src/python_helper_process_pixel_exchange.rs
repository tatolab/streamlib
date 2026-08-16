// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The helper child's half of the pixel exchange.
//!
//! A Python processor's pixels live in the engine's pools, one process away.
//! Acquiring goes through two channels that already exist: the escalate
//! socket carries the request (the parent allocates, checks the buffer into
//! its surface-share service, and answers with the minted surface id), and
//! the surface-share socket carries the memory itself — the checkout's
//! `recvmsg` lands the DMA-BUF plane fds in this process's fd table via
//! SCM_RIGHTS, and the consumer-side Vulkan import maps them. The escalate
//! socket never carries fds.
//!
//! Pool allocations are DMA-BUF-flavoured, and external device APIs
//! import OPAQUE_FD — one allocation cannot export both on NVIDIA. So
//! the device side goes through the parent's per-surface staging buffer:
//! `open_device_export_staging` has the parent publish that staging and
//! its refill timeline, the checkout delivers both fds, CUDA imports the
//! memory, and each refill is an escalate round trip whose answer is the
//! timeline value to wait for. The host's own wait after a refill orders
//! nothing for this process; the timeline does.

use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::PyDict;

#[cfg(target_os = "linux")]
use pyo3::exceptions::PyRuntimeError;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use parking_lot::Mutex;
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, ConsumerVulkanDevice, ConsumerVulkanTimelineSemaphore,
};

#[cfg(target_os = "linux")]
use crate::python_cuda_pixel_exchange::CudaImportedSurface;

use streamlib::sdk::rhi::PixelFormat;

/// Warn through the child's own log module.
///
/// This process installs no tracing subscriber, so `tracing` here reaches
/// nobody; `streamlib.log` rides the escalate `Log` op into the unified JSONL.
#[cfg(target_os = "linux")]
fn warn_through_the_childs_log_module(python: Python<'_>, message: String) {
    let _ = python
        .import("streamlib.log")
        .and_then(|log_module| log_module.call_method1("warn", (message,)));
}

/// One escalate round trip to the parent, called with the GIL attached.
///
/// The callable is the bridge's `request_from_parent`, whose wait on the
/// response releases the GIL — a slow parent parks this thread, never the
/// interpreter's others.
#[cfg(target_os = "linux")]
fn escalate_round_trip_to_parent<'py>(
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

/// One field of an escalate response, named in the failure so a parent
/// that answered a shape this child does not understand says which part.
#[cfg(target_os = "linux")]
fn response_field<'py>(response: &Bound<'py, PyAny>, field: &str) -> PyResult<Bound<'py, PyAny>> {
    response.get_item(field).map_err(|_| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's response carried no {field}"
        ))
    })
}

/// A u64 the wire carries as a decimal string, because JSON has no 64-bit
/// integer. Host-side counterpart: `EscalateResponseOk::staging_byte_size`.
#[cfg(target_os = "linux")]
fn decimal_string_field(response: &Bound<'_, PyAny>, field: &str) -> PyResult<u64> {
    let as_written: String = response_field(response, field)?.extract()?;
    as_written.parse().map_err(|_| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's {field} was {as_written:?}, which is not a decimal u64"
        ))
    })
}

/// The exporting device's UUID, as 32 hex characters.
#[cfg(target_os = "linux")]
fn parse_device_uuid(as_hex: &str) -> PyResult<[u8; 16]> {
    let mut uuid = [0u8; 16];
    if as_hex.len() != 32 {
        return Err(crate::python_processor_context::gpu_operation_error(
            format!(
                "the parent reported the exporting device UUID as {as_hex:?}, which is not 32 hex \
             characters; importing onto the wrong GPU reads the wrong memory rather than failing"
            ),
        ));
    }
    for (byte, hex_pair) in uuid.iter_mut().zip(as_hex.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(hex_pair).unwrap_or("zz"), 16).map_err(
            |_| {
                crate::python_processor_context::gpu_operation_error(format!(
                    "the parent reported the exporting device UUID as {as_hex:?}, which is not hex"
                ))
            },
        )?;
    }
    Ok(uuid)
}

/// Raise the service's own refusal of a checkout, if it refused one.
///
/// `checked_out_subject` names what was being checked out, because the caller
/// knows whether it asked for a published surface or the staging behind one and
/// the response does not. Taken as `format_args!` so the happy path — every
/// frame a consumer claims or resolves — formats nothing.
#[cfg(target_os = "linux")]
fn refuse_check_out_the_service_declined(
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

/// A pooled texture the parent acquired on this child's behalf.
///
/// Deliberately not a [`HelperCheckedOutPixelSurface`]: no fds were checked
/// out and no memory is mapped here. What this carries is the name a dispatch
/// binds and a downstream processor resolves, and the debt that hands the
/// pool slot back.
#[cfg(target_os = "linux")]
pub(crate) struct HelperAcquiredTexture {
    pub(crate) surface_id: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format_name: String,
    pub(crate) release_to_parent: HelperSurfaceReleaseDebt,
}

/// What a checkout turned into once the fds were imported: mapped memory
/// plus the layout facts every view derives from.
#[cfg(target_os = "linux")]
pub(crate) struct HelperCheckedOutPixelSurface {
    /// The id this surface travels under — what a downstream processor
    /// resolves, and what keys the parent's registry entry.
    pub(crate) surface_id: String,
    pub(crate) consumer_buffer: ConsumerVulkanBuffer,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) bytes_per_row: u64,
    /// Present only on an acquired surface — a resolved one belongs to its
    /// acquirer, and releasing it here would evict somebody else's frame.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    /// The checkout lease this surface owes, whoever owns the surface itself.
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    pub(crate) release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
    /// The plane fds this checkout was delivered, kept so
    /// `export_dma_buf` can answer from them. They are the same fds a
    /// host-side export would mint — the check-out is a kernel dup of
    /// that export — so the child answers locally instead of asking for
    /// something it already holds.
    exported_plane_fds: Vec<OwnedFd>,
    /// The client this surface was checked out through, and the one its
    /// device export goes back to.
    pub(crate) exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[cfg(target_os = "linux")]
impl HelperCheckedOutPixelSurface {
    /// A DMA-BUF fd for the first plane, and the plane's byte size.
    ///
    /// The fd is a `dup` of the one this process was handed at check-out,
    /// so the caller owns it and closing it does not disturb this
    /// surface's own mapping.
    pub(crate) fn export_dma_buf(&self) -> PyResult<(RawFd, u64)> {
        let first_plane_fd = self.exported_plane_fds.first().ok_or_else(|| {
            PyRuntimeError::new_err(
                "this surface was checked out with no plane fd to export; nothing to hand to \
                 native code",
            )
        })?;
        let exported = first_plane_fd.try_clone().map_err(|duplicate_failure| {
            PyRuntimeError::new_err(format!(
                "could not duplicate this surface's DMA-BUF fd: {duplicate_failure}"
            ))
        })?;
        Ok((
            exported.into_raw_fd(),
            self.bytes_per_row * u64::from(self.height),
        ))
    }
}

/// What the parent answered when asked to open a surface's device export.
///
/// A struct rather than eight positional arguments: `staging_byte_size`
/// and `bytes_per_row` are both `u64` and `width`/`height` are both `u32`,
/// so a transposition compiles clean and lands as a wrong-sized CUDA
/// import or a wrong stride.
#[cfg(target_os = "linux")]
struct DeviceExportStagingDescription {
    /// The surface-share id the staging and its timeline are published
    /// under — what the check-out names, not the source surface's id.
    staging_share_id: String,
    staging_byte_size: u64,
    exporting_device_uuid: [u8; 16],
    width: u32,
    height: u32,
    format: PixelFormat,
    bytes_per_row: u64,
    writable: bool,
}

/// What a helper process holds of a surface's device export: CUDA's
/// import of the parent's staging buffer, and the timeline every refill
/// signals.
///
/// The staging itself belongs to the parent's `GpuContext` and is cached
/// there per surface. This is the consumer half — one import per surface
/// per child, memoised on the exchange client, because
/// `cudaImportExternalMemory` is not a per-frame cost.
#[cfg(target_os = "linux")]
pub(crate) struct HelperDeviceExport {
    pub(crate) cuda_import: Arc<CudaImportedSurface>,
    refill_done: ConsumerVulkanTimelineSemaphore,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) bytes_per_row: u64,
    pub(crate) writable: bool,
}

/// The release an acquired surface owes its parent: one `release_handle`
/// escalate op, which drops the parent registry's strong reference and the
/// surface-share service entry together.
#[cfg(target_os = "linux")]
pub(crate) struct HelperSurfaceReleaseDebt {
    escalate_request_to_parent: Py<PyAny>,
    handle_id: String,
}

#[cfg(target_os = "linux")]
impl Drop for HelperSurfaceReleaseDebt {
    /// Best-effort: a parent that is already gone has released everything
    /// with the connection, so a failure here is logged, never raised.
    fn drop(&mut self) {
        Python::attach(|python| {
            let release_outcome: PyResult<()> = (|| {
                let op = PyDict::new(python);
                op.set_item("op", "release_handle")?;
                op.set_item("handle_id", self.handle_id.as_str())?;
                escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
                Ok(())
            })();
            if let Err(release_failure) = release_outcome {
                warn_through_the_childs_log_module(
                    python,
                    format!(
                        "releasing surface {} to the parent failed ({release_failure}); its pool \
                         slot returns at teardown",
                        self.handle_id
                    ),
                );
            }
        });
    }
}

/// The checkout lease a surface owes the surface-share service: one
/// `release_check_out`, over the connection the checkout was minted on.
///
/// Unlike [`HelperSurfaceReleaseDebt`] this unregisters nothing — it says only
/// "I am done reading". Owned by the surface, so it settles when the surface's
/// `GpuSurfaceOwnedMemory` loses its last share, handle *and* every exported
/// view: paying it at `close()` would return the slot under a live tensor.
#[cfg(target_os = "linux")]
pub(crate) struct HelperSurfaceCheckOutLeaseDebt {
    exchange_client: Arc<HelperProcessGpuExchangeClient>,
    surface_id: String,
}

#[cfg(target_os = "linux")]
impl Drop for HelperSurfaceCheckOutLeaseDebt {
    /// Best-effort: a parent that is already gone dropped this connection, and
    /// the service reclaims every lease on a connection's socket closing — so
    /// a failure here is logged, never raised.
    fn drop(&mut self) {
        Python::attach(|python| {
            // The round trip blocks, so it runs detached — this can be a
            // capsule deleter running under the child's GIL.
            let released =
                python.detach(|| self.exchange_client.release_check_out(&self.surface_id));
            if let Err(release_failure) = released {
                warn_through_the_childs_log_module(
                    python,
                    format!(
                        "releasing the checkout of surface {} failed ({release_failure}); its pool \
                         slot returns when this helper's connection closes",
                        self.surface_id
                    ),
                );
            }
        });
    }
}

/// The child-side client that fulfills `ctx.gpu_limited_access` calls by
/// crossing to the parent: escalate for allocation, surface-share for the
/// memory, one consumer Vulkan device per child for the import.
pub(crate) struct HelperProcessGpuExchangeClient {
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    escalate_request_to_parent: Py<PyAny>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    surface_socket_path: PathBuf,
    /// One connection per child, opened at first checkout. Taken out for
    /// each exchange and put back only on success, so a stream with half a
    /// frame in it is dropped rather than reused.
    #[cfg(target_os = "linux")]
    surface_share_connection: Mutex<Option<UnixStream>>,
    /// One Vulkan device per child, created at first import.
    #[cfg(target_os = "linux")]
    consumer_vulkan_device: Mutex<Option<Arc<ConsumerVulkanDevice>>>,
    /// Device exports memoised per surface id: the CUDA import and the
    /// timeline import are per-surface setup costs, never per-frame ones.
    ///
    /// Keyed by the source surface's id and held for this child's
    /// lifetime. The parent can evict a staging (its surface was
    /// unregistered), and this side cannot observe that — the next
    /// refill's escalate round trip fails by name instead, which is the
    /// honest answer to a surface that is gone.
    #[cfg(target_os = "linux")]
    device_exports_by_surface: Mutex<std::collections::HashMap<String, Arc<HelperDeviceExport>>>,
}

/// Bound on the wait for a refill the parent said it signalled. The copy
/// is VRAM→VRAM; reaching this bound means the parent's queue is wedged,
/// not that the copy is slow.
#[cfg(target_os = "linux")]
const DEVICE_EXPORT_REFILL_WAIT_TIMEOUT_NS: u64 = 2_000_000_000;

impl HelperProcessGpuExchangeClient {
    pub(crate) fn new(escalate_request_to_parent: Py<PyAny>, surface_socket_path: PathBuf) -> Self {
        Self {
            escalate_request_to_parent,
            surface_socket_path,
            #[cfg(target_os = "linux")]
            surface_share_connection: Mutex::new(None),
            #[cfg(target_os = "linux")]
            consumer_vulkan_device: Mutex::new(None),
            #[cfg(target_os = "linux")]
            device_exports_by_surface: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Ask the parent to allocate, then check the result out and import it.
    ///
    /// Called attached; the escalate wait releases the GIL, and the checkout
    /// and Vulkan import run detached.
    #[cfg(target_os = "linux")]
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
            escalate_request_to_parent: self.escalate_request_to_parent.clone_ref(python),
            handle_id: handle_id.clone(),
        };
        let mut checked_out = python.detach(|| self.check_out_and_import(&handle_id))?;
        checked_out.release_to_parent = Some(release_to_parent);
        Ok(checked_out)
    }

    /// Check out a surface another processor published. No release debt:
    /// the surface belongs to its acquirer.
    #[cfg(target_os = "linux")]
    pub(crate) fn resolve_surface(
        self: &Arc<Self>,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        python.detach(|| self.check_out_and_import(surface_id))
    }

    /// Claim a published surface against producer reuse, without importing
    /// its memory.
    ///
    /// The cheap half of [`Self::resolve_surface`]: the checkout is what mints
    /// the lease, and a holder that only needs the frame to hold still owes no
    /// Vulkan import for it. The plane fds the service delivers alongside the
    /// claim close with this call.
    #[cfg(target_os = "linux")]
    pub(crate) fn claim_surface_against_producer_reuse(
        self: &Arc<Self>,
        surface_id: &str,
    ) -> PyResult<HelperSurfaceCheckOutLeaseDebt> {
        let (response, _plane_fds_closed_by_scope) = self.check_out_surface(surface_id)?;
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), &response)?;
        Ok(HelperSurfaceCheckOutLeaseDebt {
            exchange_client: Arc::clone(self),
            surface_id: surface_id.to_string(),
        })
    }

    /// `check_out` over the surface-share socket, then the DMA-BUF import.
    #[cfg(target_os = "linux")]
    fn check_out_and_import(
        self: &Arc<Self>,
        surface_id: &str,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        let (response, received_fds) = self.check_out_surface(surface_id)?;
        self.import_checked_out_surface(surface_id, &response, received_fds)
    }

    /// Wait for the parent's GPU device to go idle.
    ///
    /// A real wait, not an acknowledgement: the parent runs it inside its
    /// escalate scope, so the reply means the device was idle on that
    /// side — which is the only side there is.
    #[cfg(target_os = "linux")]
    pub(crate) fn wait_device_idle(&self, python: Python<'_>) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "wait_device_idle")?;
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Acquire a pooled texture, and take back the surface id the parent
    /// minted for it plus the extent it actually allocated.
    ///
    /// Nothing is imported: the id is what a kernel dispatch binds and what a
    /// downstream processor resolves. Mapping the texture's memory into this
    /// process is a separate capability.
    #[cfg(target_os = "linux")]
    pub(crate) fn acquire_texture(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        wire_format_name: &str,
        usage: &[String],
    ) -> PyResult<HelperAcquiredTexture> {
        let op = PyDict::new(python);
        op.set_item("op", "acquire_texture")?;
        op.set_item("width", width)?;
        op.set_item("height", height)?;
        op.set_item("format", wire_format_name)?;
        op.set_item("usage", usage)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let surface_id: String = response_field(&response, "handle_id")?.extract()?;
        // The debt exists from the moment the parent allocated: dropping it
        // hands the pool slot back rather than stranding it. Bound before the
        // metadata extraction below, so a malformed response still pays the
        // release — the same ordering `acquire_pixel_buffer` documents.
        let release_to_parent = HelperSurfaceReleaseDebt {
            escalate_request_to_parent: self.escalate_request_to_parent.clone_ref(python),
            handle_id: surface_id.clone(),
        };
        Ok(HelperAcquiredTexture {
            width: response_field(&response, "width")?.extract()?,
            height: response_field(&response, "height")?.extract()?,
            format_name: response_field(&response, "format")?.extract()?,
            release_to_parent,
            surface_id,
        })
    }

    /// Build a compute kernel in the parent and take back its id plus the
    /// binding shape reflection found.
    ///
    /// The shape comes back because dispatch resolves by name and only the
    /// shader knows which kind each name is — without it this side would have
    /// to guess a kind for every binding it supplies.
    #[cfg(target_os = "linux")]
    pub(crate) fn register_compute_kernel(
        &self,
        python: Python<'_>,
        spv_hex: &str,
        push_constant_size: u32,
        declared_bindings: &Bound<'_, PyAny>,
    ) -> PyResult<(
        String,
        Vec<crate::python_processor_context::ReflectedComputeBinding>,
    )> {
        let op = PyDict::new(python);
        op.set_item("op", "register_compute_kernel")?;
        op.set_item("spv_hex", spv_hex)?;
        op.set_item("push_constant_size", push_constant_size)?;
        op.set_item("bindings", declared_bindings)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let kernel_id: String = response_field(&response, "handle_id")?.extract()?;
        let mut reflected = Vec::new();
        for entry in response_field(&response, "bindings")?.try_iter()? {
            let entry = entry?;
            reflected.push(crate::python_processor_context::ReflectedComputeBinding {
                name: entry.get_item("name")?.extract()?,
                kind: entry.get_item("kind")?.extract()?,
            });
        }
        Ok((kernel_id, reflected))
    }

    /// Dispatch a registered compute kernel with its bindings supplied by name.
    ///
    /// Returns when the parent's dispatch has retired: compute is synchronous
    /// host-side, so the writes are visible on return and no timeline value
    /// crosses back for this side to wait on.
    #[cfg(target_os = "linux")]
    pub(crate) fn run_compute_kernel(
        &self,
        python: Python<'_>,
        kernel_id: &str,
        bindings: &Bound<'_, PyAny>,
        push_constants_hex: &str,
        group_count: (u32, u32, u32),
    ) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "run_compute_kernel")?;
        op.set_item("kernel_id", kernel_id)?;
        op.set_item("bindings", bindings)?;
        op.set_item("push_constants_hex", push_constants_hex)?;
        op.set_item("group_count_x", group_count.0)?;
        op.set_item("group_count_y", group_count.1)?;
        op.set_item("group_count_z", group_count.2)?;
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Open this surface's device export, importing the parent's staging
    /// into CUDA on first ask and memoising it for this child.
    ///
    /// Two channels again, same division as the host path: escalate asks
    /// the parent to allocate and publish the staging, and the
    /// surface-share check-out carries the memory — the staging's
    /// OPAQUE_FD and the refill timeline's fd, in that order.
    #[cfg(target_os = "linux")]
    pub(crate) fn open_device_export(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<Arc<HelperDeviceExport>> {
        // Memoised per pool slot: the parent's staging (and this CUDA
        // import of it) spans every frame the slot publishes, while each
        // refill names — and the parent validates — the specific frame id.
        let source_pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        if let Some(already_open) = self
            .device_exports_by_surface
            .lock()
            .get(source_pool_slot_key)
        {
            return Ok(Arc::clone(already_open));
        }

        let op = PyDict::new(python);
        op.set_item("op", "open_device_export_staging")?;
        op.set_item("surface_id", surface_id)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let format_name: String = response_field(&response, "format")?.extract()?;
        let exporting_device_uuid: String =
            response_field(&response, "exporting_device_uuid")?.extract()?;
        let described = DeviceExportStagingDescription {
            staging_share_id: response_field(&response, "handle_id")?.extract()?,
            staging_byte_size: decimal_string_field(&response, "staging_byte_size")?,
            exporting_device_uuid: parse_device_uuid(&exporting_device_uuid)?,
            width: response_field(&response, "width")?.extract()?,
            height: response_field(&response, "height")?.extract()?,
            format: crate::python_processor_context::parse_pixel_format_name(&format_name)?,
            bytes_per_row: decimal_string_field(&response, "bytes_per_row")?,
            writable: response_field(&response, "writable")?.extract()?,
        };

        let opened = python.detach(|| -> PyResult<Arc<HelperDeviceExport>> {
            Ok(Arc::new(
                self.check_out_and_import_device_export(&described)?,
            ))
        })?;
        Ok(Arc::clone(
            self.device_exports_by_surface
                .lock()
                .entry(source_pool_slot_key.to_string())
                .or_insert(opened),
        ))
    }

    /// Ask the parent to run one device-export copy and wait for the
    /// timeline value it answers with.
    ///
    /// The wait is the whole point of the round trip: the parent's own
    /// post-submit wait orders nothing for this process, so a read that
    /// skipped this would race the copy it asked for.
    #[cfg(target_os = "linux")]
    pub(crate) fn run_device_export_copy(
        &self,
        python: Python<'_>,
        escalate_op: &str,
        surface_id: &str,
        export: &HelperDeviceExport,
    ) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", escalate_op)?;
        op.set_item("surface_id", surface_id)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let signalled = decimal_string_field(&response, "timeline_value")?;
        python.detach(|| {
            export
                .refill_done
                .wait(signalled, DEVICE_EXPORT_REFILL_WAIT_TIMEOUT_NS)
                .map_err(|wait_failure| {
                    crate::python_processor_context::gpu_operation_error(format!(
                        "waiting for {escalate_op} of surface {surface_id:?} to reach timeline \
                         value {signalled} failed: {wait_failure}"
                    ))
                })
        })
    }

    /// Check the published staging out and import it: the memory into
    /// CUDA, the timeline into this child's Vulkan device.
    #[cfg(target_os = "linux")]
    fn check_out_and_import_device_export(
        &self,
        described: &DeviceExportStagingDescription,
    ) -> PyResult<HelperDeviceExport> {
        let staging_share_id = described.staging_share_id.as_str();
        // The staging's own claim is never released: it is memoised for this
        // child's lifetime and is escalate-allocated rather than pool-backed,
        // so the lease pins no producer's slot. A pool-backed staging would
        // owe a debt here.
        let (response, received_fds) = self.check_out_surface(staging_share_id)?;
        refuse_check_out_the_service_declined(
            format_args!("the device-export staging {staging_share_id:?}"),
            &response,
        )?;
        let handle_type = response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf");
        if handle_type != "opaque_fd" {
            return Err(crate::python_processor_context::gpu_operation_error(
                format!(
                    "the device-export staging {staging_share_id:?} is registered as \
                 {handle_type:?}; an external device API imports OPAQUE_FD, and importing one \
                 flavour through the other hands the driver a handle of the wrong type"
                ),
            ));
        }
        // The staging's memory fd, then the refill timeline's — the order
        // the registration published them in.
        let [staging_fd, refill_done_fd] =
            <[OwnedFd; 2]>::try_from(received_fds).map_err(|delivered: Vec<OwnedFd>| {
                crate::python_processor_context::gpu_operation_error(format!(
                    "check_out of the device-export staging {staging_share_id:?} returned {} \
                     fds; it carries exactly the staging's memory and its refill timeline",
                    delivered.len(),
                ))
            })?;

        let vulkan_device = self.consumer_vulkan_device()?;
        // Both imports adopt their fd on success and leave it with the
        // caller on failure, so each is handed over only at its call.
        let refill_done = match ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(
            &vulkan_device,
            refill_done_fd.as_raw_fd(),
        ) {
            Ok(imported_timeline) => {
                let _adopted_by_vulkan = refill_done_fd.into_raw_fd();
                imported_timeline
            }
            Err(import_failure) => {
                return Err(crate::python_processor_context::gpu_operation_error(
                    format!(
                        "this helper could not import the refill timeline of \
                     {staging_share_id:?}: {import_failure}"
                    ),
                ));
            }
        };
        let cuda_import = crate::python_cuda_pixel_exchange::import_opaque_fd_into_cuda(
            staging_fd,
            described.staging_byte_size,
            described.exporting_device_uuid,
        )
        .map(Arc::new)
        .map_err(crate::python_processor_context::gpu_operation_error)?;

        Ok(HelperDeviceExport {
            cuda_import,
            refill_done,
            width: described.width,
            height: described.height,
            format: described.format,
            bytes_per_row: described.bytes_per_row,
            writable: described.writable,
        })
    }

    /// One request/response over the cached surface-share connection,
    /// reconnecting lazily. The connection is taken out of the slot for the
    /// exchange and put back only on success, so a stream with half a frame
    /// in it is structurally dropped rather than remembered to be.
    #[cfg(target_os = "linux")]
    fn surface_share_request(
        &self,
        request: &serde_json::Value,
    ) -> PyResult<(serde_json::Value, Vec<OwnedFd>)> {
        let mut connection = self.surface_share_connection.lock();
        let stream = match connection.take() {
            Some(open_stream) => open_stream,
            None => streamlib_surface_client::connect_to_surface_share_socket(
                &self.surface_socket_path,
            )
            .map_err(|connect_failure| {
                PyRuntimeError::new_err(format!(
                    "could not reach the surface-share socket at {}: {connect_failure}. The \
                     parent runtime owns that socket; if it is gone, this helper is orphaned",
                    self.surface_socket_path.display(),
                ))
            })?,
        };
        let (response, received_raw_fds) = streamlib_surface_client::send_request_with_fds(
            &stream,
            request,
            &[],
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|io_failure| {
            PyRuntimeError::new_err(format!(
                "the surface-share request failed mid-stream: {io_failure}"
            ))
        })?;
        *connection = Some(stream);
        // SAFETY: adopting kernel-delivered fds the recvmsg just placed in
        // this process's fd table; nothing else holds them.
        let received_fds = received_raw_fds
            .into_iter()
            .map(|raw_fd| unsafe { OwnedFd::from_raw_fd(raw_fd) })
            .collect();
        Ok((response, received_fds))
    }

    /// Claim a surface against producer reuse and take its plane fds.
    ///
    /// The one place this op is spelled: a checkout is what pins the frame,
    /// and every caller owes the matching [`Self::release_check_out`].
    #[cfg(target_os = "linux")]
    fn check_out_surface(&self, surface_id: &str) -> PyResult<(serde_json::Value, Vec<OwnedFd>)> {
        self.surface_share_request(&serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        }))
    }

    /// Let go of one claim on a surface, freeing its slot for its producer.
    #[cfg(target_os = "linux")]
    fn release_check_out(&self, surface_id: &str) -> PyResult<(serde_json::Value, Vec<OwnedFd>)> {
        self.surface_share_request(&serde_json::json!({
            "op": "release_check_out",
            "surface_id": surface_id,
        }))
    }

    /// Validate the checkout metadata and turn the plane fds into mapped
    /// memory. The fds are `OwnedFd`s, so every early return closes them by
    /// scope rather than by remembering to.
    #[cfg(target_os = "linux")]
    fn import_checked_out_surface(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        received_fds: Vec<OwnedFd>,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), response)?;

        // Trailing timeline-semaphore fds arrive after the plane fds when the
        // registration carried them. A pixel-buffer checkout carries none
        // today; peeled rather than assumed absent, so a registration that
        // gains them cannot corrupt the plane list.
        let trailing_timeline_fd_count = ["has_produce_done_fd", "has_consume_done_fd"]
            .into_iter()
            .filter(|flag| {
                response
                    .get(flag)
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            })
            .count();
        if received_fds.len() < trailing_timeline_fd_count + 1 {
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} returned {} fds, fewer than the {} its metadata \
                 promises",
                received_fds.len(),
                trailing_timeline_fd_count + 1,
            )));
        }
        let mut plane_fds = received_fds;
        drop(plane_fds.split_off(plane_fds.len() - trailing_timeline_fd_count));

        let handle_type = response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf");
        if handle_type != "dma_buf" {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} is registered as {handle_type:?}, which is not a \
                 host-mappable pixel buffer: an opaque_fd surface belongs to the device-export \
                 path and imports through CUDA, not a CPU mapping"
            )));
        }

        let required_positive_u32_metadata_field = |field: &str| -> PyResult<u32> {
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
        };
        let width = required_positive_u32_metadata_field("width")?;
        let height = required_positive_u32_metadata_field("height")?;
        let format_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let format = crate::python_processor_context::parse_pixel_format_name(format_name)?;
        let plane_sizes: Vec<u64> = response
            .get("plane_sizes")
            .and_then(|value| value.as_array())
            .map(|sizes| sizes.iter().filter_map(|size| size.as_u64()).collect())
            .unwrap_or_default();
        if plane_sizes.len() != plane_fds.len() {
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} returned {} plane fds but {} plane sizes",
                plane_fds.len(),
                plane_sizes.len(),
            )));
        }
        // The allocation's row pitch, padding included — the same derivation
        // the engine-side view uses, so the strides agree across processes.
        let plane0_size = plane_sizes.first().copied().unwrap_or(0);
        if plane0_size == 0 || !plane0_size.is_multiple_of(u64::from(height)) {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} reports plane size {plane0_size}, not a whole number of \
                 {height} rows"
            )));
        }
        let bytes_per_row = plane0_size / u64::from(height);

        let vulkan_device = self.consumer_vulkan_device()?;
        // Import from dups: vkAllocateMemory takes ownership of a fd only on
        // success, so handing over the originals would leave their ownership
        // ambiguous on a partial multi-plane failure. The originals close by
        // scope; a dup not consumed by a failed import is the leak accepted
        // on that error path.
        let dup_fds: Vec<OwnedFd> = plane_fds
            .iter()
            .map(|plane_fd| {
                plane_fd.try_clone().map_err(|duplicate_failure| {
                    PyRuntimeError::new_err(format!(
                        "could not duplicate a plane fd for import: {duplicate_failure}"
                    ))
                })
            })
            .collect::<PyResult<_>>()?;
        // Ownership hands over here: nothing can fail between the unwrap to
        // raw fds and the import call that adopts them.
        let dup_raw_fds: Vec<RawFd> = dup_fds.into_iter().map(OwnedFd::into_raw_fd).collect();
        let consumer_buffer = match ConsumerVulkanBuffer::from_dma_buf_fds(
            &vulkan_device,
            &dup_raw_fds,
            &plane_sizes,
        ) {
            Ok(imported_buffer) => imported_buffer,
            Err(import_failure) => {
                // Vulkan takes fd ownership only on success, so a refused
                // single-plane fd is ours to close — and this path can run
                // per frame. A multi-plane failure leaves the tail's
                // ownership ambiguous (already-imported planes were freed by
                // the callee's teardown) and those dups leak, bounded by
                // plane count; no multi-plane pool surface exists today.
                if let [only_plane_fd] = dup_raw_fds[..] {
                    // SAFETY: an fd Vulkan refused ownership of; ours alone.
                    unsafe { libc::close(only_plane_fd) };
                }
                return Err(PyRuntimeError::new_err(format!(
                    "Vulkan could not import surface {surface_id:?}'s DMA-BUF planes: \
                     {import_failure}"
                )));
            }
        };

        Ok(HelperCheckedOutPixelSurface {
            surface_id: surface_id.to_string(),
            consumer_buffer,
            width,
            height,
            format,
            bytes_per_row,
            release_to_parent: None,
            release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt {
                exchange_client: Arc::clone(self),
                surface_id: surface_id.to_string(),
            },
            exported_plane_fds: plane_fds,
            exchange_client: Arc::clone(self),
        })
    }

    #[cfg(target_os = "linux")]
    fn consumer_vulkan_device(&self) -> PyResult<Arc<ConsumerVulkanDevice>> {
        let mut device = self.consumer_vulkan_device.lock();
        if let Some(existing_device) = device.as_ref() {
            return Ok(Arc::clone(existing_device));
        }
        let created = Arc::new(ConsumerVulkanDevice::new().map_err(|device_failure| {
            PyRuntimeError::new_err(format!(
                "this helper process could not create its Vulkan import device: {device_failure}"
            ))
        })?);
        *device = Some(Arc::clone(&created));
        Ok(created)
    }
}

/// The lease against a real surface-share service.
///
/// The RAII floor of the lifetime contract: a checkout claims the frame, and
/// the debt's drop releases it. Provable without a GPU — the wheel's device
/// tests are `requires_gpu` and CI declares no GPU runner, so the lease's
/// balance has to hold here or it is not protected anywhere.
#[cfg(all(test, target_os = "linux"))]
mod surface_check_out_lease_debt_tests {
    use super::*;
    use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

    /// The client needs an escalate callable it never uses here — the lease
    /// path speaks only to the surface-share socket.
    fn exchange_client_on(share: &SurfaceShareUnderTest) -> Arc<HelperProcessGpuExchangeClient> {
        Python::initialize();
        Python::attach(|python| {
            Arc::new(HelperProcessGpuExchangeClient::new(
                python.None(),
                share.socket_path.clone(),
            ))
        })
    }

    /// The checkout claims the frame; dropping the debt — the last share of
    /// the surface going away — is what returns the slot to its producer.
    #[test]
    fn a_check_out_is_held_until_its_debt_drops() {
        let share = SurfaceShareUnderTest::start("debt");
        let surface_id = share.publish_one_surface();
        let exchange_client = exchange_client_on(&share);

        let (response, _plane_fds_closed_by_scope) = exchange_client
            .check_out_surface(&surface_id)
            .expect("the checkout round trip");
        assert!(
            response.get("error").is_none(),
            "the service refused the checkout: {response}"
        );
        let debt = HelperSurfaceCheckOutLeaseDebt {
            exchange_client: Arc::clone(&exchange_client),
            surface_id: surface_id.clone(),
        };
        assert_eq!(
            share.outstanding_claims_on(&surface_id),
            1,
            "a checked-out frame is claimed against producer reuse"
        );

        drop(debt);
        assert_eq!(
            share.outstanding_claims_on(&surface_id),
            0,
            "the slot returns to its producer when the last share lets go"
        );
    }

    /// The claim a cast takes: the same lease the resolve path mints, without
    /// the memory — an object that only needs the frame to hold still owes no
    /// Vulkan import for it, which is also why this is provable with no GPU.
    #[test]
    fn a_claim_pins_the_frame_without_importing_it() {
        let share = SurfaceShareUnderTest::start("claim");
        let surface_id = share.publish_one_surface();
        let exchange_client = exchange_client_on(&share);

        let claim = exchange_client
            .claim_surface_against_producer_reuse(&surface_id)
            .expect("the claim round trip");
        assert_eq!(share.outstanding_claims_on(&surface_id), 1);

        // Claims are counted: a second one on the same surface is its own, and
        // releasing it leaves the first standing.
        let second_claim = exchange_client
            .claim_surface_against_producer_reuse(&surface_id)
            .expect("a second claim on one surface");
        assert_eq!(share.outstanding_claims_on(&surface_id), 2);
        drop(second_claim);
        assert_eq!(
            share.outstanding_claims_on(&surface_id),
            1,
            "one holder letting go must not release another holder's claim"
        );

        drop(claim);
        assert_eq!(share.outstanding_claims_on(&surface_id), 0);
    }

    /// #1872 as the wheel sees it: a frame the producer recycled is not a
    /// claim to be taken quietly. The typed cast on a stale bag must raise —
    /// pinning the slot's *current* frame while the caller believes it pinned
    /// the delivered one would be the same silent wrongness one layer up.
    #[test]
    fn claiming_a_recycled_frame_is_refused_naming_the_recycling() {
        let share = SurfaceShareUnderTest::start("recycled");
        share.publish_pool_slot_frame("pool-slot-under-test", 1);
        let exchange_client = exchange_client_on(&share);

        let claim_while_current = exchange_client
            .claim_surface_against_producer_reuse("pool-slot-under-test#1")
            .expect("the current frame claims");
        assert_eq!(share.outstanding_claims_on("pool-slot-under-test"), 1);
        drop(claim_while_current);

        // The producer laps the pool: generation 2 publishes, 1 retires.
        share.publish_pool_slot_frame("pool-slot-under-test", 2);

        let Err(refusal) =
            exchange_client.claim_surface_against_producer_reuse("pool-slot-under-test#1")
        else {
            panic!("claiming a recycled frame must refuse");
        };
        assert!(
            refusal.to_string().contains("recycled"),
            "the refusal must say the frame was recycled: {refusal}"
        );
        assert_eq!(
            share.outstanding_claims_on("pool-slot-under-test"),
            0,
            "a refused claim must leave no lease behind"
        );

        exchange_client
            .claim_surface_against_producer_reuse("pool-slot-under-test#2")
            .expect("the new current frame claims");
    }

    /// A surface the service does not know is not a claim to be taken quietly:
    /// the caller decides what an unclaimable frame means, so the refusal has
    /// to reach it.
    #[test]
    fn claiming_a_surface_the_service_does_not_know_is_refused_by_name() {
        let share = SurfaceShareUnderTest::start("unknown");
        let exchange_client = exchange_client_on(&share);

        let Err(refusal) = exchange_client.claim_surface_against_producer_reuse("no-such-surface")
        else {
            panic!("claiming a surface the service does not know must refuse");
        };
        assert!(
            refusal.to_string().contains("no-such-surface"),
            "the refusal must name the surface: {refusal}"
        );
    }
}
