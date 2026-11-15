// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Install the engine's own surface store on this context — the engine's
    /// — as both its surface store and the one the shared pools and export
    /// stagings register through. Installed once; a second is refused.
    pub(crate) fn install_the_engines_surface_store(&self, store: SurfaceStore) -> Result<()> {
        self.engines_own_surface_store
            .set(store.clone())
            .map_err(|_| {
                Error::GpuError(
                    "the engine's own surface store is installed once, and it already was".into(),
                )
            })?;
        *self
            .surface_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store);
        Ok(())
    }

    /// The engine's own surface store, `None` until the engine installs it.
    pub(crate) fn engines_own_surface_store(&self) -> Option<&SurfaceStore> {
        self.engines_own_surface_store.get()
    }

    /// Clear this view's surface store, closing its connection to the
    /// surface-sharing service. Called as the stream whose view this is stops.
    pub(crate) fn clear_surface_store(&self) {
        // A poisoned slot is still cleared: a stream's stop must not panic on
        // a lock another thread's panic left behind.
        *self
            .surface_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.surface_store.lock().unwrap().clone()
    }
}
