// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a checkout turns into on this side, and the debts that settle it:
//! the release an acquire owes its parent, the checkout lease, and the claim
//! that holds a frame against producer reuse.

use std::sync::Arc;

use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;

use super::{
    GpuPoolSlotFrameClaimOfThisImporter, GpuSurfaceGeometry, HelperCheckedOutStorageBuffer,
    HelperCheckedOutTextureSurface, HelperProcessGpuExchangeClient, PixelSurfaceGeometry,
    hand_a_release_handle_to_the_release_worker,
};
#[cfg(target_os = "linux")]
use super::{
    HelperAcquiredTexture, HelperForeignSurfaceUnregisterDebt, HelperPixelBufferPoolSlotImport,
};
#[cfg(target_os = "macos")]
use super::{
    HelperIOSurfaceCpuLock, HelperIOSurfacePixelBufferPoolSlotImport, HelperIOSurfaceUseCountClaim,
};

/// The refusal every pixel-shaped door gives a tensor surface.
pub(crate) fn a_tensor_surface_is_not_a_pixel_surface() -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(
        "this surface is a tensor storage buffer, not pixels: it has a shape and a dtype, no \
         width, height, format or CPU mapping, and its one door is `__dlpack__` \
         (`torch.from_dlpack`)",
    )
}

/// What a checkout turned into once its memory was imported: mapped memory
/// plus the layout facts every view derives from.
pub(crate) struct HelperCheckedOutPixelSurface {
    /// The id this surface travels under — what a downstream processor
    /// resolves, and what keys the parent's registry entry.
    pub(crate) surface_id: String,
    /// The pool slot's import, shared with this helper's per-slot cache and
    /// every other frame checked out over the slot.
    #[cfg(target_os = "linux")]
    pub(crate) pixel_buffer_pool_slot_import: Arc<HelperPixelBufferPoolSlotImport>,
    /// The pool slot's IOSurface and its import, shared with this helper's
    /// per-slot cache and every other frame checked out over the slot.
    #[cfg(target_os = "macos")]
    pub(crate) iosurface_pool_slot_import: Arc<HelperIOSurfacePixelBufferPoolSlotImport>,
    /// This frame's claim on the IOSurface's use count — the claim the kernel
    /// keeps truthful across processes, and drops if this process dies.
    #[cfg(target_os = "macos")]
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    pub(crate) iosurface_use_count_claim: HelperIOSurfaceUseCountClaim,
    /// The IOSurface lock this surface's CPU access holds, if any.
    #[cfg(target_os = "macos")]
    pub(crate) iosurface_cpu_lock: HelperIOSurfaceCpuLock,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) bytes_per_row: u64,
    /// Present only on an acquired surface — a resolved one belongs to its
    /// acquirer, and releasing it here would evict somebody else's frame.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    /// Present only on an adopted foreign DMA-BUF — the registration this
    /// import created is this surface's to remove; settled by its own Drop.
    #[cfg(target_os = "linux")]
    pub(crate) unregister_foreign_from_surface_share: Option<HelperForeignSurfaceUnregisterDebt>,
    /// What holds this surface against producer reuse, whoever owns the
    /// surface itself.
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    pub(crate) held_against_producer_reuse: HelperSurfaceHeldAgainstProducerReuse,
    /// The client this surface was checked out through, and the one its
    /// device export goes back to.
    #[cfg(target_os = "linux")]
    pub(crate) exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

impl HelperCheckedOutPixelSurface {
    /// The host view of this surface's own mapping; null where the import
    /// has no host mapping.
    pub(crate) fn host_visible_pixel_plane_view(
        &self,
    ) -> crate::python_gpu_surface_pixel_exchange::HostVisiblePixelPlaneView {
        crate::python_gpu_surface_pixel_exchange::HostVisiblePixelPlaneView {
            base_address: self.host_mapped_base_address(),
            bytes_per_row: self.bytes_per_row,
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }
}

/// The backings one surface id can stand for, behind one lifetime story:
/// the pixel buffer, texture and tensor storage buffer a checkout imports,
/// and the acquired device texture that was never checked out at all — a
/// name whose memory stays engine-side.
pub(crate) enum HelperCheckedOutSurface {
    PixelBuffer(HelperCheckedOutPixelSurface),
    Texture(HelperCheckedOutTextureSurface),
    #[cfg(target_os = "linux")]
    AcquiredDeviceTexture(HelperAcquiredTexture),
    StorageBuffer(HelperCheckedOutStorageBuffer),
}

impl HelperCheckedOutSurface {
    pub(crate) fn surface_id(&self) -> &str {
        match self {
            Self::PixelBuffer(pixel_surface) => &pixel_surface.surface_id,
            Self::Texture(texture_surface) => &texture_surface.surface_id,
            #[cfg(target_os = "linux")]
            Self::AcquiredDeviceTexture(acquired_texture) => &acquired_texture.surface_id,
            Self::StorageBuffer(storage_buffer) => &storage_buffer.surface_id,
        }
    }

    /// Whether this surface is pixels or a tensor, and its geometry.
    pub(crate) fn geometry(&self) -> GpuSurfaceGeometry {
        let pixels = |width, height, format_wire_name| {
            GpuSurfaceGeometry::Pixels(PixelSurfaceGeometry {
                width,
                height,
                format_wire_name,
            })
        };
        match self {
            Self::PixelBuffer(pixel_surface) => pixels(
                pixel_surface.width,
                pixel_surface.height,
                pixel_surface.format.wire_name(),
            ),
            Self::Texture(texture_surface) => pixels(
                texture_surface.width,
                texture_surface.height,
                texture_surface.format.wire_name(),
            ),
            #[cfg(target_os = "linux")]
            Self::AcquiredDeviceTexture(acquired_texture) => pixels(
                acquired_texture.width,
                acquired_texture.height,
                acquired_texture.format.wire_name(),
            ),
            Self::StorageBuffer(storage_buffer) => {
                GpuSurfaceGeometry::TensorStorageBuffer(storage_buffer.tensor_layout.clone())
            }
        }
    }

    /// The tensor storage buffer this surface is, if it is one.
    pub(crate) fn tensor_storage_buffer(&self) -> Option<&HelperCheckedOutStorageBuffer> {
        match self {
            Self::StorageBuffer(storage_buffer) => Some(storage_buffer),
            _ => None,
        }
    }
}

/// The release an acquired surface owes its parent: one `release_handle`
/// escalate op, which drops the parent registry's strong reference and the
/// surface-share service entry together.
pub(crate) struct HelperSurfaceReleaseDebt {
    pub(super) release_to_parent_without_waiting: Py<PyAny>,
    pub(super) handle_id: String,
}

impl Drop for HelperSurfaceReleaseDebt {
    /// Best-effort: a parent that is already gone has released everything
    /// with the connection, so a failure here is logged, never raised.
    fn drop(&mut self) {
        Python::attach(|python| {
            if let Err(release_failure) = hand_a_release_handle_to_the_release_worker(
                python,
                &self.release_to_parent_without_waiting,
                &self.handle_id,
            ) {
                tracing::warn!(
                    "releasing surface {} to the parent failed ({release_failure}); its pool \
                     slot returns at teardown",
                    self.handle_id
                );
            }
        });
    }
}

/// The checkout lease a surface owes the surface-share service: one
/// `release_check_out`, over this helper's current connection. The service
/// frees only a lease the asking connection took, so a lease taken on a
/// connection set aside after a timeout is freed by nothing until this helper
/// stops.
///
/// Unlike [`HelperSurfaceReleaseDebt`] this unregisters nothing — it says only
/// "I am done reading". Owned by the surface, so it settles when the surface's
/// `GpuSurfaceOwnedMemory` loses its last share, handle *and* every exported
/// view: paying it at `close()` would return the slot under a live tensor.
pub(crate) struct HelperSurfaceCheckOutLeaseDebt {
    pub(super) exchange_client: Arc<HelperProcessGpuExchangeClient>,
    pub(super) surface_id: String,
}

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
                tracing::warn!(
                    "releasing the checkout of surface {} failed ({release_failure}); its pool \
                     slot returns when the connection it was claimed on closes, at the latest \
                     when this helper stops",
                    self.surface_id
                );
            }
        });
    }
}

/// What holds a surface still against producer reuse: a checkout lease, or —
/// for a pooled frame its pool's claim table covers — a claim in that table.
pub(crate) enum HelperSurfaceHeldAgainstProducerReuse {
    CheckOutLease(
        #[expect(
            dead_code,
            reason = "settled by its own Drop; nothing reads it, and that is the point"
        )]
        HelperSurfaceCheckOutLeaseDebt,
    ),
    GpuPoolClaim(
        #[expect(
            dead_code,
            reason = "the claim is released by its own Drop; nothing reads it, and that is the point"
        )]
        Arc<GpuPoolSlotFrameClaimOfThisImporter>,
    ),
}
