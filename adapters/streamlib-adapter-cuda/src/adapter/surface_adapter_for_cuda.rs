// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `SurfaceAdapter` impl for `CudaSurfaceAdapter`.

use std::marker::PhantomData;
use std::sync::Arc;

use streamlib_consumer_rhi::{DevicePrivilege, VulkanRhiDevice, VulkanTimelineSemaphoreLike};
use streamlib_surface_adapter::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceId, SurfaceRegistration,
    WriteGuard,
};

use crate::view::{CudaReadView, CudaWriteView};

use super::{ConsumeDoneSignal, CudaSurfaceAdapter};

impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for CudaSurfaceAdapter<D> {
    type ReadView<'g> = CudaReadView<'g>;
    type WriteView<'g> = CudaWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let acquired = match self.try_begin_read(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: "writer".to_string(),
                });
            }
        };
        let (buffer, size) = self.finalize_read(surface.id, acquired)?;
        Ok(ReadGuard::new(
            self,
            surface.id,
            CudaReadView {
                buffer,
                size,
                _marker: PhantomData,
            },
        ))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let acquired = match self.try_begin_write(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: self.surfaces.describe_contention(surface.id),
                });
            }
        };
        let (buffer, size) = self.finalize_write(surface.id, acquired)?;
        Ok(WriteGuard::new(
            self,
            surface.id,
            CudaWriteView {
                buffer,
                size,
                _marker: PhantomData,
            },
        ))
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        let acquired = match self.try_begin_read(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let (buffer, size) = self.finalize_read(surface.id, acquired)?;
        Ok(Some(ReadGuard::new(
            self,
            surface.id,
            CudaReadView {
                buffer,
                size,
                _marker: PhantomData,
            },
        )))
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        let acquired = match self.try_begin_write(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let (buffer, size) = self.finalize_write(surface.id, acquired)?;
        Ok(Some(WriteGuard::new(
            self,
            surface.id,
            CudaWriteView {
                buffer,
                size,
                _marker: PhantomData,
            },
        )))
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        // Consumer-side release: signal `consume_done` so the producer
        // can wait on it before re-writing. Inner Option: `None` means
        // "not the last reader, skip signal". Outer Option: `None`
        // means "surface raced an unregister".
        let signal: Option<Option<ConsumeDoneSignal<D>>> =
            self.surfaces.with_mut(surface_id, |state| {
                debug_assert!(state.read_holders > 0, "read release without acquire");
                state.dec_read_holders();
                if state.read_holders > 0 {
                    return None;
                }
                let next = state.next_signal_value();
                state.current_signal_value = next;
                Some((Arc::clone(&state.consume_done), next))
            });
        let signal = match signal {
            Some(s) => s,
            None => {
                tracing::warn!(
                    ?surface_id,
                    "end_read_access on unknown surface — racing unregister"
                );
                return;
            }
        };
        if let Some((consume_done, value)) = signal
            && let Err(e) = consume_done.signal_host(value)
        {
            tracing::error!(?surface_id, %value, %e, "consume_done signal failed on read release");
        }
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Producer-side release: signal `produce_done` so the consumer
        // can wait on it before reading.
        let signal: Option<(
            Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>,
            u64,
        )> = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.write_held, "write release without acquire");
            state.set_write_held(false);
            let next = state.next_signal_value();
            state.current_signal_value = next;
            (Arc::clone(&state.produce_done), next)
        });
        let (produce_done, value) = match signal {
            Some(s) => s,
            None => {
                tracing::warn!(
                    ?surface_id,
                    "end_write_access on unknown surface — racing unregister"
                );
                return;
            }
        };
        if let Err(e) = produce_done.signal_host(value) {
            tracing::error!(?surface_id, %value, %e, "produce_done signal failed on write release");
        }
    }
}
