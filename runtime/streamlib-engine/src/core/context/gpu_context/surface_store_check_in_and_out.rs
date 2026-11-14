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

    /// Check in a pixel buffer to the surface-share service, returning a surface ID.
    ///
    /// The surface ID can be shared with other processes (e.g., Python subprocesses)
    /// which can then call `check_out_surface` to get the same IOSurface.
    ///
    /// If this pixel buffer was already checked in, returns the existing ID.
    #[cfg(target_os = "macos")]
    pub fn check_in_surface(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        let store = self.surface_store.lock().unwrap();
        let store = store.as_ref().ok_or_else(|| {
            crate::core::Error::Configuration(
                NO_SURFACE_STORE_IS_CONNECTED_TO_THIS_GPU_CONTEXT.into(),
            )
        })?;
        store.check_in(pixel_buffer)
    }

    /// Check out a surface by ID, returning the pixel buffer.
    ///
    /// Returns from local cache if available, otherwise looks the surface up
    /// over the surface-share service's Mach channel and imports its
    /// IOSurface, zero-copy.
    #[cfg(target_os = "macos")]
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        let store = self.surface_store.lock().unwrap();
        let store = store.as_ref().ok_or_else(|| {
            crate::core::Error::Configuration(
                NO_SURFACE_STORE_IS_CONNECTED_TO_THIS_GPU_CONTEXT.into(),
            )
        })?;
        store.check_out(surface_id)
    }

    /// Check in a pixel buffer (non-macOS stub).
    #[cfg(not(target_os = "macos"))]
    pub fn check_in_surface(&self, _pixel_buffer: &PixelBuffer) -> Result<String> {
        Err(crate::core::Error::NotSupported(
            "Surface store is only supported on macOS".into(),
        ))
    }

    /// Check out a surface (non-macOS stub).
    #[cfg(not(target_os = "macos"))]
    pub fn check_out_surface(&self, _surface_id: &str) -> Result<PixelBuffer> {
        Err(crate::core::Error::NotSupported(
            "Surface store is only supported on macOS".into(),
        ))
    }
}
