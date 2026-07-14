// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RAII guards for scoped surface access.

use crate::adapter::SurfaceAdapter;
use crate::surface::SurfaceId;

/// Scoped read access to a surface.
///
/// Drop signals the release-side timeline semaphore via the adapter's
/// sealed `end_read_access` hook.
pub struct ReadGuard<'g, A: SurfaceAdapter + ?Sized> {
    adapter: &'g A,
    surface_id: SurfaceId,
    view: A::ReadView<'g>,
}

impl<'g, A: SurfaceAdapter + ?Sized> ReadGuard<'g, A> {
    /// Construct a guard. Adapter implementations call this from
    /// [`SurfaceAdapter::acquire_read`] after the acquire-side wait.
    pub fn new(adapter: &'g A, surface_id: SurfaceId, view: A::ReadView<'g>) -> Self {
        Self {
            adapter,
            surface_id,
            view,
        }
    }

    pub fn view(&self) -> &A::ReadView<'g> {
        &self.view
    }

    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }
}

impl<A: SurfaceAdapter + ?Sized> Drop for ReadGuard<'_, A> {
    fn drop(&mut self) {
        self.adapter.end_read_access(self.surface_id);
    }
}

impl<A: SurfaceAdapter + ?Sized> std::fmt::Debug for ReadGuard<'_, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadGuard")
            .field("surface_id", &self.surface_id)
            .finish_non_exhaustive()
    }
}

/// Scoped exclusive write access to a surface.
pub struct WriteGuard<'g, A: SurfaceAdapter + ?Sized> {
    adapter: &'g A,
    surface_id: SurfaceId,
    view: A::WriteView<'g>,
}

impl<'g, A: SurfaceAdapter + ?Sized> WriteGuard<'g, A> {
    pub fn new(adapter: &'g A, surface_id: SurfaceId, view: A::WriteView<'g>) -> Self {
        Self {
            adapter,
            surface_id,
            view,
        }
    }

    pub fn view(&self) -> &A::WriteView<'g> {
        &self.view
    }

    pub fn view_mut(&mut self) -> &mut A::WriteView<'g> {
        &mut self.view
    }

    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }
}

impl<A: SurfaceAdapter + ?Sized> Drop for WriteGuard<'_, A> {
    fn drop(&mut self) {
        self.adapter.end_write_access(self.surface_id);
    }
}
