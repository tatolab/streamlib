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
use streamlib::sdk::iceoryx2::{FRAME_HEADER_SIZE, QueuedBagDeparture, QueuedBagObserver};

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

/// The claims a helper child holds on the GPU surfaces its queued bags name.
///
/// `resolve_surface` is user-facing, so without this the claim on a delivered
/// frame could not begin until user code reached for it — a bag waiting its
/// turn behind a slow callback was unprotected for the whole wait, and the
/// producer's pool recycled its slot underneath. Installed on the child's
/// input mailboxes, which report every bag that enters and leaves.
///
/// The bag's own keys are the only place a surface can be named: the wire
/// carries no type information by design, so this reads the one key the
/// handoff contract fixes — `surface_id` — and a value the service does not
/// know is simply not a surface.
#[cfg(target_os = "linux")]
pub(crate) struct HelperProcessQueuedBagSurfaceClaims {
    exchange_client: Arc<HelperProcessGpuExchangeClient>,
    ledger: Mutex<HelperProcessSurfaceClaimLedger>,
}

/// Which claims are on bags still queued and which are on the bag the
/// processor is reading, under one lock so a claim is never in neither.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct HelperProcessSurfaceClaimLedger {
    on_queued_bags: Vec<String>,
    /// Released when the callback returns rather than at the read, so user
    /// code that reads a bag and resolves its surface a moment later never
    /// falls into a gap between the two.
    on_bags_the_processor_is_reading: Vec<String>,
}

#[cfg(target_os = "linux")]
impl HelperProcessSurfaceClaimLedger {
    fn take_queued_claim(&mut self, surface_id: &str) -> bool {
        let Some(position) = self
            .on_queued_bags
            .iter()
            .position(|claimed| claimed == surface_id)
        else {
            return false;
        };
        self.on_queued_bags.swap_remove(position);
        true
    }

    fn move_queued_claim_to_the_processor(&mut self, surface_id: &str) {
        if self.take_queued_claim(surface_id) {
            self.on_bags_the_processor_is_reading
                .push(surface_id.to_string());
        }
    }
}

#[cfg(target_os = "linux")]
impl HelperProcessQueuedBagSurfaceClaims {
    pub(crate) fn new(exchange_client: Arc<HelperProcessGpuExchangeClient>) -> Self {
        Self {
            exchange_client,
            ledger: Mutex::new(HelperProcessSurfaceClaimLedger::default()),
        }
    }

    /// Release every claim taken for a bag the processor has now finished
    /// with. Called once per `process()` return.
    pub(crate) fn release_every_claim_the_processor_has_finished_with(&self) {
        // Taken out from under the lock first: each release is a socket round
        // trip, and none of them belongs inside it.
        let finished_with = {
            let mut ledger = self.ledger.lock();
            std::mem::take(&mut ledger.on_bags_the_processor_is_reading)
        };
        for surface_id in finished_with {
            self.release_one_claim(&surface_id);
        }
    }

    fn release_one_claim(&self, surface_id: &str) {
        if let Err(release_failure) = self.exchange_client.release_check_out(surface_id) {
            Python::attach(|python| {
                warn_through_the_childs_log_module(
                    python,
                    format!(
                        "releasing the claim on queued surface {surface_id} failed \
                         ({release_failure}); its pool slot returns when this helper's \
                         surface-share connection closes"
                    ),
                );
            });
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for HelperProcessQueuedBagSurfaceClaims {
    /// Teardown owes a release for everything still held, whether or not the
    /// mailboxes got to report their departures first.
    fn drop(&mut self) {
        self.release_every_claim_the_processor_has_finished_with();
        let still_queued = std::mem::take(&mut self.ledger.lock().on_queued_bags);
        for surface_id in still_queued {
            self.release_one_claim(&surface_id);
        }
    }
}

#[cfg(target_os = "linux")]
impl QueuedBagObserver for HelperProcessQueuedBagSurfaceClaims {
    fn bag_queued(&self, wire_frame: &[u8]) {
        for surface_id in surface_ids_named_by_wire_frame(wire_frame) {
            // The fds a successful checkout returns close with the response:
            // this claim is about the frame staying still, not about reading
            // it.
            match self.exchange_client.check_out_surface(&surface_id) {
                Ok((response, _plane_fds_closed_by_scope)) if response.get("error").is_none() => {
                    self.ledger.lock().on_queued_bags.push(surface_id);
                }
                // A refusal is the ordinary answer for a string that merely
                // looks like a surface id.
                Ok(_) => {}
                Err(checkout_failure) => Python::attach(|python| {
                    warn_through_the_childs_log_module(
                        python,
                        format!(
                            "could not claim queued surface {surface_id} ({checkout_failure}); \
                             its producer may recycle the frame before this processor reads it"
                        ),
                    );
                }),
            }
        }
    }

    fn bag_departed(&self, wire_frame: &[u8], departure: QueuedBagDeparture) {
        match departure {
            QueuedBagDeparture::DeliveredToProcessor => {
                // Asking for another bag means the processor is done with the
                // last one. This is the release point every execution mode
                // reaches: a manual-mode processor drives itself and runs no
                // host loop, so nothing sweeps after a callback it never
                // makes, and without this its claims would pin the
                // producer's slots for the child's whole life.
                //
                // The cost is that a callback reading two ports releases the
                // first bag's claim at the second read. That bag has already
                // been handed over, so it is back to the protection it had
                // before any of this — never worse.
                self.release_every_claim_the_processor_has_finished_with();
                let mut ledger = self.ledger.lock();
                for surface_id in surface_ids_named_by_wire_frame(wire_frame) {
                    ledger.move_queued_claim_to_the_processor(&surface_id);
                }
            }
            // A bag the queue threw away owes its release now — nobody will
            // ever read it, and an unreleased claim pins a producer's slot
            // until this helper's connection closes.
            QueuedBagDeparture::DiscardedUnread => {
                for surface_id in surface_ids_named_by_wire_frame(wire_frame) {
                    if self.ledger.lock().take_queued_claim(&surface_id) {
                        self.release_one_claim(&surface_id);
                    }
                }
            }
        }
    }
}

/// The surface ids a wire frame's bag names, in no particular order.
///
/// Reads the bag as msgpack and collects every string under a `surface_id`
/// key, at any depth. Nothing else in the bag is examined and no type is
/// inferred — this is the handoff key, not a schema.
///
/// Borrows rather than decoding: a bag may carry inline `bytes`, and this runs
/// twice per bag on the receive path, so materializing the tree would copy
/// every payload just to find a string.
#[cfg(target_os = "linux")]
fn surface_ids_named_by_wire_frame(wire_frame: &[u8]) -> Vec<String> {
    const SURFACE_ID_BAG_KEY: &str = "surface_id";

    let Some(bag_bytes) = wire_frame.get(FRAME_HEADER_SIZE..) else {
        return Vec::new();
    };
    // msgpack stores a map key as its literal bytes, so a bag whose encoding
    // does not contain them cannot name a surface however it is shaped. Most
    // bags on most ports carry no surface at all, and this spares them the
    // walk entirely.
    if !bag_bytes
        .windows(SURFACE_ID_BAG_KEY.len())
        .any(|window| window == SURFACE_ID_BAG_KEY.as_bytes())
    {
        return Vec::new();
    }
    let Ok(bag) = rmpv::decode::read_value_ref(&mut &bag_bytes[..]) else {
        return Vec::new();
    };

    fn utf8_of<'value>(value: &'value rmpv::ValueRef<'_>) -> Option<&'value str> {
        match value {
            rmpv::ValueRef::String(text) => text.as_str(),
            _ => None,
        }
    }

    let mut named = Vec::new();
    let mut unvisited = vec![bag];
    while let Some(value) = unvisited.pop() {
        match value {
            rmpv::ValueRef::Map(entries) => {
                for (key, entry) in entries {
                    let names_a_surface = utf8_of(&key) == Some(SURFACE_ID_BAG_KEY);
                    if names_a_surface {
                        if let Some(surface_id) = utf8_of(&entry) {
                            named.push(surface_id.to_string());
                            continue;
                        }
                    }
                    unvisited.push(entry);
                }
            }
            rmpv::ValueRef::Array(entries) => unvisited.extend(entries),
            _ => {}
        }
    }
    named
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
        if let Some(already_open) = self.device_exports_by_surface.lock().get(surface_id) {
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
        // Published under the *source* surface's id: that is what a
        // handle knows, and what every later refill names.
        Ok(Arc::clone(
            self.device_exports_by_surface
                .lock()
                .entry(surface_id.to_string())
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
        if let Some(checkout_error) = response.get("error").and_then(|value| value.as_str()) {
            return Err(crate::python_processor_context::gpu_operation_error(
                format!(
                    "the surface-share service refused check_out of the device-export staging \
                 {staging_share_id:?}: {checkout_error}"
                ),
            ));
        }
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
        if let Some(checkout_error) = response.get("error").and_then(|value| value.as_str()) {
            return Err(PyRuntimeError::new_err(format!(
                "the surface-share service refused check_out of {surface_id:?}: {checkout_error}"
            )));
        }

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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn wire_frame_carrying(bag: rmpv::Value) -> Vec<u8> {
        let mut frame = vec![0u8; FRAME_HEADER_SIZE];
        rmpv::encode::write_value(&mut frame, &bag).expect("encode the bag");
        frame
    }

    fn map(entries: Vec<(&str, rmpv::Value)>) -> rmpv::Value {
        rmpv::Value::Map(
            entries
                .into_iter()
                .map(|(key, value)| (rmpv::Value::from(key), value))
                .collect(),
        )
    }

    #[test]
    fn a_video_frame_bag_names_its_surface() {
        let frame = wire_frame_carrying(map(vec![
            ("surface_id", rmpv::Value::from("frame-7")),
            ("width", rmpv::Value::from(1920)),
            ("height", rmpv::Value::from(1080)),
        ]));
        assert_eq!(surface_ids_named_by_wire_frame(&frame), vec!["frame-7"]);
    }

    /// A bag is a self-describing map with no schema, so a producer may nest
    /// or repeat the handoff key. Every one is a frame somebody will read.
    #[test]
    fn nested_and_repeated_surface_keys_are_all_claimed() {
        let frame = wire_frame_carrying(map(vec![
            ("surface_id", rmpv::Value::from("frame-7")),
            (
                "frames",
                rmpv::Value::Array(vec![
                    map(vec![("surface_id", rmpv::Value::from("frame-8"))]),
                    map(vec![("surface_id", rmpv::Value::from("frame-9"))]),
                ]),
            ),
        ]));
        let mut named = surface_ids_named_by_wire_frame(&frame);
        named.sort();
        assert_eq!(named, vec!["frame-7", "frame-8", "frame-9"]);
    }

    /// Nothing else in the bag is examined, and no type is inferred: a bag
    /// with no handoff key names no surface, and a non-string under one is
    /// not an id.
    #[test]
    fn a_bag_without_a_string_surface_key_names_nothing() {
        let no_key = wire_frame_carrying(map(vec![("samples", rmpv::Value::from(48_000))]));
        assert!(surface_ids_named_by_wire_frame(&no_key).is_empty());

        let wrong_type = wire_frame_carrying(map(vec![("surface_id", rmpv::Value::from(7))]));
        assert!(surface_ids_named_by_wire_frame(&wrong_type).is_empty());
    }

    /// Inline `bytes` in a bag are the reason this borrows rather than
    /// decoding — the scan runs twice per bag and must not copy a payload to
    /// find a string.
    #[test]
    fn a_bag_carrying_inline_bytes_still_names_its_surface() {
        let frame = wire_frame_carrying(map(vec![
            ("surface_id", rmpv::Value::from("frame-7")),
            ("thumbnail", rmpv::Value::Binary(vec![0xab; 4096])),
        ]));
        assert_eq!(surface_ids_named_by_wire_frame(&frame), vec!["frame-7"]);
    }

    /// A frame too short to hold a header, or carrying bytes that are not
    /// msgpack, claims nothing rather than panicking on the receive path.
    #[test]
    fn an_unreadable_frame_claims_nothing() {
        assert!(surface_ids_named_by_wire_frame(&[0u8; 4]).is_empty());

        let mut not_msgpack = vec![0u8; FRAME_HEADER_SIZE];
        not_msgpack.extend_from_slice(&[0xc1, 0xc1, 0xc1]);
        assert!(surface_ids_named_by_wire_frame(&not_msgpack).is_empty());
    }
}

/// The claim ledger against a real surface-share service.
///
/// Every path here is one a rig test cannot reach: the wheel's GPU tests are
/// `requires_gpu` and CI declares no GPU runner, so the ledger's balance —
/// every claim taken answered exactly once — has to be provable without a
/// device. Nothing below imports a surface or touches a GPU; the service is
/// the only thing under test alongside the ledger.
#[cfg(all(test, target_os = "linux"))]
mod claim_ledger_tests {
    use super::*;
    use std::os::unix::io::{FromRawFd as _, IntoRawFd as _};
    use streamlib::sdk::engine::linux_surface_share::{
        SurfaceShareState, UnixSocketSurfaceService,
    };

    /// A service, and the lease table the pool would read.
    struct SurfaceShareUnderTest {
        _service: UnixSocketSurfaceService,
        socket_path: std::path::PathBuf,
        check_out_leases: Arc<streamlib::sdk::context::SurfaceCheckOutLeaseRegistry>,
        _socket_dir: std::path::PathBuf,
    }

    impl Drop for SurfaceShareUnderTest {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self._socket_dir);
        }
    }

    fn start_surface_share(label: &str) -> SurfaceShareUnderTest {
        let socket_dir = std::env::temp_dir().join(format!(
            "streamlib-claim-ledger-{}-{}",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&socket_dir);
        std::fs::create_dir_all(&socket_dir).expect("a directory for the test socket");
        let socket_path = socket_dir.join("surface-share.sock");

        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("the surface-share service starts");
        std::thread::sleep(std::time::Duration::from_millis(50));

        SurfaceShareUnderTest {
            _service: service,
            socket_path,
            check_out_leases,
            _socket_dir: socket_dir,
        }
    }

    /// Publish one surface into the service and return the id it lives under.
    fn publish_one_surface(share: &SurfaceShareUnderTest) -> String {
        let name = std::ffi::CString::new("streamlib-claim-ledger-test").unwrap();
        let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(raw_fd >= 0, "memfd_create failed");
        let backing = unsafe { std::fs::File::from_raw_fd(raw_fd) };
        backing.set_len(4096).expect("size the backing memfd");
        let backing_fd = backing.into_raw_fd();

        let publisher =
            streamlib_surface_client::connect_to_surface_share_socket(&share.socket_path)
                .expect("a publisher connection");
        let (response, _no_reply_fds) = streamlib_surface_client::send_request_with_fds(
            &publisher,
            &serde_json::json!({
                "op": "check_in",
                "runtime_id": "claim-ledger-test-runtime",
                "width": 32,
                "height": 32,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
            }),
            &[backing_fd],
            0,
        )
        .expect("check_in");
        unsafe { libc::close(backing_fd) };
        // Held open deliberately: closing it would take the surface with it.
        std::mem::forget(publisher);
        response
            .get("surface_id")
            .and_then(|value| value.as_str())
            .expect("the service minted a surface id")
            .to_string()
    }

    fn wire_frame_naming(surface_id: &str) -> Vec<u8> {
        let mut frame = vec![0u8; FRAME_HEADER_SIZE];
        rmpv::encode::write_value(
            &mut frame,
            &rmpv::Value::Map(vec![(
                rmpv::Value::from("surface_id"),
                rmpv::Value::from(surface_id),
            )]),
        )
        .expect("encode the bag");
        frame
    }

    /// The client the claims check out through. Returned alongside so the
    /// connection outlives the ledger — a dropped connection would reclaim
    /// every lease and hide whatever the ledger got wrong.
    fn claims_against(
        share: &SurfaceShareUnderTest,
    ) -> (
        Arc<HelperProcessGpuExchangeClient>,
        HelperProcessQueuedBagSurfaceClaims,
    ) {
        // The client needs an escalate callable it never uses here — the
        // claim path speaks only to the surface-share socket.
        Python::initialize();
        let client = Python::attach(|python| {
            Arc::new(HelperProcessGpuExchangeClient::new(
                python.None(),
                share.socket_path.clone(),
            ))
        });
        let claims = HelperProcessQueuedBagSurfaceClaims::new(Arc::clone(&client));
        (client, claims)
    }

    fn outstanding(share: &SurfaceShareUnderTest, surface_id: &str) -> u32 {
        share
            .check_out_leases
            .outstanding_check_out_count(surface_id)
            .expect("the lease table stays readable")
    }

    /// The whole point: a queued bag's frame is claimed before anyone reads
    /// it, stays claimed while the processor has it, and is let go after.
    #[test]
    fn a_queued_bag_is_claimed_on_arrival_and_released_after_the_callback() {
        let share = start_surface_share("callback");
        let surface_id = publish_one_surface(&share);
        let (_client, claims) = claims_against(&share);
        let frame = wire_frame_naming(&surface_id);

        claims.bag_queued(&frame);
        assert_eq!(
            outstanding(&share, &surface_id),
            1,
            "a bag waiting its turn must already be protected"
        );

        claims.bag_departed(&frame, QueuedBagDeparture::DeliveredToProcessor);
        assert_eq!(
            outstanding(&share, &surface_id),
            1,
            "the claim outlives the read, so user code can resolve the surface after it"
        );

        claims.release_every_claim_the_processor_has_finished_with();
        assert_eq!(outstanding(&share, &surface_id), 0);
    }

    /// A bag the queue threw away is never read, so its claim owes an
    /// immediate release — the leak that would otherwise cost one pinned slot
    /// per dropped frame on a lagging latest-wins port.
    #[test]
    fn a_bag_discarded_unread_releases_its_claim_at_once() {
        let share = start_surface_share("discarded");
        let surface_id = publish_one_surface(&share);
        let (_client, claims) = claims_against(&share);
        let frame = wire_frame_naming(&surface_id);

        claims.bag_queued(&frame);
        claims.bag_departed(&frame, QueuedBagDeparture::DiscardedUnread);
        assert_eq!(outstanding(&share, &surface_id), 0);
    }

    /// A manual-mode processor drives itself and runs no host loop, so
    /// nothing ever sweeps after a callback. Its claims must still balance,
    /// or its producer's pool fills to the cap and never recovers.
    ///
    /// Mental-revert: release only from the host loop's post-`process()`
    /// sweep and the first frame's claim is still outstanding here.
    #[test]
    fn a_processor_that_never_sweeps_still_lets_go_of_the_bag_before_last() {
        let share = start_surface_share("manual");
        let first_surface_id = publish_one_surface(&share);
        let second_surface_id = publish_one_surface(&share);
        let (_client, claims) = claims_against(&share);
        let first = wire_frame_naming(&first_surface_id);
        let second = wire_frame_naming(&second_surface_id);

        claims.bag_queued(&first);
        claims.bag_departed(&first, QueuedBagDeparture::DeliveredToProcessor);
        claims.bag_queued(&second);
        claims.bag_departed(&second, QueuedBagDeparture::DeliveredToProcessor);

        assert_eq!(
            outstanding(&share, &first_surface_id),
            0,
            "asking for another bag means the processor is done with the last one"
        );
        assert_eq!(
            outstanding(&share, &second_surface_id),
            1,
            "the bag it is reading now stays protected"
        );
    }

    /// Teardown owes a release for everything still held, on both sides of
    /// the ledger.
    #[test]
    fn dropping_the_ledger_releases_everything_it_was_holding() {
        let share = start_surface_share("teardown");
        let queued_surface_id = publish_one_surface(&share);
        let reading_surface_id = publish_one_surface(&share);
        let (_client, claims) = claims_against(&share);
        let queued = wire_frame_naming(&queued_surface_id);
        let being_read = wire_frame_naming(&reading_surface_id);

        claims.bag_queued(&being_read);
        claims.bag_departed(&being_read, QueuedBagDeparture::DeliveredToProcessor);
        claims.bag_queued(&queued);
        assert_eq!(outstanding(&share, &queued_surface_id), 1);
        assert_eq!(outstanding(&share, &reading_surface_id), 1);

        drop(claims);
        assert_eq!(outstanding(&share, &queued_surface_id), 0);
        assert_eq!(outstanding(&share, &reading_surface_id), 0);
    }

    /// A bag naming no surface costs the service nothing — the common case on
    /// an audio or control port, which must not pay for the GPU path.
    #[test]
    fn a_bag_naming_no_surface_claims_nothing() {
        let share = start_surface_share("surfaceless");
        let surface_id = publish_one_surface(&share);
        let (_client, claims) = claims_against(&share);

        let mut frame = vec![0u8; FRAME_HEADER_SIZE];
        rmpv::encode::write_value(
            &mut frame,
            &rmpv::Value::Map(vec![(
                rmpv::Value::from("samples"),
                rmpv::Value::from(48_000),
            )]),
        )
        .expect("encode the bag");

        claims.bag_queued(&frame);
        claims.bag_departed(&frame, QueuedBagDeparture::DeliveredToProcessor);
        assert_eq!(outstanding(&share, &surface_id), 0);
    }
}
