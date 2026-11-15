// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `SurfaceAdapter` for `CpuReadbackSurfaceAdapter`: the acquire and end-access verbs.

use std::sync::Arc;

use streamlib_consumer_rhi::{VulkanRhiDevice, VulkanTimelineSemaphoreLike};
use streamlib_surface_adapter::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceId, SurfaceRegistration,
    WriteGuard,
};

use streamlib_consumer_rhi::VulkanLayout;

use super::{
    ConsumeDoneSignalOnReadRelease, CopyDirection, CpuReadbackSurfaceAdapter, build_read_view,
    build_write_view,
};
use crate::view::{CpuReadbackReadView, CpuReadbackWriteView};

impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for CpuReadbackSurfaceAdapter<D> {
    type ReadView<'g> = CpuReadbackReadView<'g>;
    type WriteView<'g> = CpuReadbackWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let outcome = self
            .acquire_inner(surface.id, false, true)?
            .expect("blocking acquire returned None");
        Ok(ReadGuard::new(
            self,
            surface.id,
            build_read_view(&outcome.snap),
        ))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let outcome = self
            .acquire_inner(surface.id, true, true)?
            .expect("blocking acquire returned None");
        Ok(WriteGuard::new(
            self,
            surface.id,
            build_write_view(&outcome.snap),
        ))
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        match self.acquire_inner(surface.id, false, false)? {
            Some(o) => Ok(Some(ReadGuard::new(
                self,
                surface.id,
                build_read_view(&o.snap),
            ))),
            None => Ok(None),
        }
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        match self.acquire_inner(surface.id, true, false)? {
            Some(o) => Ok(Some(WriteGuard::new(
                self,
                surface.id,
                build_write_view(&o.snap),
            ))),
            None => Ok(None),
        }
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        // Consumer-side release: signal `consume_done` so the
        // producer can wait on it before reusing the staging buffer.
        // Single-writer-per-edge per
        // the single-writer-per-edge timeline rule — the
        // consumer process is the only writer of `consume_done`; the
        // producer signals `produce_done` through the trigger's GPU
        // submit. No multi-writer race (the pre-#562 defang fixed a
        // shared-timeline race that no longer exists under
        // dual-timeline).
        //
        // `None` means the surface raced an unregister.
        let consume_done_signal: Option<ConsumeDoneSignalOnReadRelease<D::Privilege>> =
            self.surfaces.with_mut(surface_id, |state| {
                debug_assert!(state.read_holders > 0, "read release without acquire");
                state.dec_read_holders();
                if state.read_holders > 0 {
                    return ConsumeDoneSignalOnReadRelease::NotTheLastReader;
                }
                ConsumeDoneSignalOnReadRelease::LastReaderMustSignal {
                    consume_done: Arc::clone(&state.consume_done),
                    signal_sequence: Arc::clone(&state.signal_sequence),
                }
            });
        let consume_done_signal = match consume_done_signal {
            Some(s) => s,
            None => {
                tracing::warn!(?surface_id, "end_read_access on unknown surface");
                return;
            }
        };
        if let ConsumeDoneSignalOnReadRelease::LastReaderMustSignal {
            consume_done,
            signal_sequence,
        } = consume_done_signal
        {
            // Bind the reservation rather than inlining it: the
            // sequence must stay locked across the signal, or a copy
            // submit racing this release can take the same value.
            let reserved = signal_sequence.reserve_next_signal_value();
            let value = reserved.value();
            if let Err(e) = consume_done.signal_host(value) {
                tracing::error!(?surface_id, %value, %e, "consume_done signal failed on read release");
            }
        }
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Snapshot the work to do under the lock, then run the
        // trigger unlocked.
        let snap = self.surfaces.with_mut(surface_id, |state| {
            debug_assert!(state.write_held, "write release without acquire");
            Self::snapshot_surface_for_copy_submit(state)
        });
        let snap = match snap {
            Some(s) => s,
            None => {
                tracing::warn!(?surface_id, "end_write_access on unknown surface");
                return;
            }
        };

        if let Err(e) =
            self.submit_copy_and_await_produce_done(surface_id, &snap, CopyDirection::BufferToImage)
        {
            tracing::error!(?surface_id, error = %e, "cpu-readback write flush failed");
            self.surfaces.rollback_write(surface_id);
            return;
        }

        self.surfaces.with_mut(surface_id, |state| {
            state.set_write_held(false);
            state.current_layout = VulkanLayout::GENERAL;
        });
    }
}
