// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Finishing an acquire: wait on the peer timeline outside the registry lock.

use std::marker::PhantomData;

use streamlib_consumer_rhi::{VulkanRhiDevice, VulkanTimelineSemaphoreLike};
use streamlib_surface_adapter::{AdapterError, SurfaceId};
use vulkanalia::vk;

use crate::view::{CudaSurfaceView, CudaTextureView};

use super::{
    CudaSurfaceAdapter, ImageReadAcquired, ImageWriteAcquired, ReadAcquired, WriteAcquired,
};

impl<D: VulkanRhiDevice> CudaSurfaceAdapter<D> {
    pub(super) fn finalize_image_read(
        &self,
        surface_id: SurfaceId,
        acquired: ImageReadAcquired<D::Privilege>,
    ) -> Result<CudaTextureView<'_>, AdapterError> {
        // Wait for whatever value the peer (`produce_done`) has already
        // been signaled to. The peer-timeline's kernel counter is the
        // source of truth — no cross-process value publishing IPC
        // needed for v1 (see
        // the single-writer-per-edge timeline rule).
        let wait_value = acquired.peer_timeline.current_value().unwrap_or(0);
        if acquired
            .peer_timeline
            .wait(wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.surfaces.rollback_read(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }
        Ok(CudaTextureView {
            image: acquired.image,
            width: acquired.width,
            height: acquired.height,
            format: acquired.format,
            _marker: PhantomData,
        })
    }

    pub(super) fn finalize_image_write(
        &self,
        surface_id: SurfaceId,
        acquired: ImageWriteAcquired<D::Privilege>,
    ) -> Result<CudaSurfaceView<'_>, AdapterError> {
        // Producer-side wait on `consume_done`'s current value — see
        // `finalize_image_read` for the kernel-counter rationale.
        let wait_value = acquired.peer_timeline.current_value().unwrap_or(0);
        if acquired
            .peer_timeline
            .wait(wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.surfaces.rollback_write(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }
        Ok(CudaSurfaceView {
            image: acquired.image,
            width: acquired.width,
            height: acquired.height,
            format: acquired.format,
            _marker: PhantomData,
        })
    }

    pub(super) fn finalize_read(
        &self,
        surface_id: SurfaceId,
        acquired: ReadAcquired<D::Privilege>,
    ) -> Result<(vk::Buffer, vk::DeviceSize), AdapterError> {
        let wait_value = acquired.peer_timeline.current_value().unwrap_or(0);
        if acquired
            .peer_timeline
            .wait(wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.surfaces.rollback_read(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }
        Ok((acquired.buffer, acquired.size))
    }

    pub(super) fn finalize_write(
        &self,
        surface_id: SurfaceId,
        acquired: WriteAcquired<D::Privilege>,
    ) -> Result<(vk::Buffer, vk::DeviceSize), AdapterError> {
        let wait_value = acquired.peer_timeline.current_value().unwrap_or(0);
        if acquired
            .peer_timeline
            .wait(wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.surfaces.rollback_write(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }
        Ok((acquired.buffer, acquired.size))
    }
}
