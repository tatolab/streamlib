// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared registry-lock + contention-counter scaffolding for surface
//! adapters. The per-framework `SurfaceState` (timeline + texture +
//! layout + plane geometry) stays in each adapter crate; what's
//! centralized here is the read/write contention state machine and
//! the `Mutex<HashMap<SurfaceId, T>>` ownership pattern that every
//! adapter shares.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::error::AdapterError;
use crate::surface::SurfaceId;

/// Per-surface state contract every adapter's `SurfaceState` impls so
/// [`Registry`] can run the contention check without knowing the
/// framework-specific fields.
///
/// Implementors store `read_holders: u64` + `write_held: bool` (the
/// existing convention). Reading + mutating these is the entire
/// contract — finalization (timeline wait, layout transition, copy)
/// stays per-adapter.
pub trait SurfaceRegistration {
    fn write_held(&self) -> bool;
    fn read_holders(&self) -> u64;
    fn set_write_held(&mut self, held: bool);
    fn inc_read_holders(&mut self);
    fn dec_read_holders(&mut self);
}

/// `Mutex<HashMap<SurfaceId, T>>` wrapper that all in-tree adapters
/// (and 3rd-party adapters that adopt the trait) share.
///
/// The [`try_begin_read`](Self::try_begin_read) /
/// [`try_begin_write`](Self::try_begin_write) helpers run a per-adapter
/// snapshot closure under the same lock that performs the contention
/// check, so the adapter's framework-specific state extraction is
/// atomic with the counter mutation. [`rollback_read`](Self::rollback_read)
/// / [`rollback_write`](Self::rollback_write) are the symmetric paths
/// used on guard-drop or post-acquire timeout.
///
/// The lock is coarse — one `Mutex` covers every registered surface —
/// but it matches the pre-extraction shape and keeps the contention
/// check trivial. Refining lock granularity can come later if a real
/// scenario surfaces it.
pub struct Registry<T: SurfaceRegistration> {
    inner: Mutex<HashMap<SurfaceId, T>>,
}

impl<T: SurfaceRegistration> Default for Registry<T> {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: SurfaceRegistration> Registry<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new surface entry. Returns `false` if `id` was already
    /// registered (the existing entry is left untouched).
    pub fn register(&self, id: SurfaceId, state: T) -> bool {
        let mut map = self.inner.lock();
        if map.contains_key(&id) {
            return false;
        }
        map.insert(id, state);
        true
    }

    /// Drop a registered surface. Returns the removed state if any so
    /// the adapter can run its destructor logic outside the lock
    /// (e.g. EGLImage destruction on the EGL make-current thread).
    pub fn unregister(&self, id: SurfaceId) -> Option<T> {
        self.inner.lock().remove(&id)
    }

    /// Run a closure with shared access to the surface state, if it's
    /// still registered. Returns `None` for unknown ids.
    pub fn with<R, F: FnOnce(&T) -> R>(&self, id: SurfaceId, f: F) -> Option<R> {
        self.inner.lock().get(&id).map(f)
    }

    /// Run a closure with mutable access to the surface state. Used by
    /// finalize-side commits (e.g. layout-transition writes) and other
    /// per-adapter state edits that aren't covered by the
    /// `try_begin_*` / `rollback_*` helpers.
    pub fn with_mut<R, F: FnOnce(&mut T) -> R>(&self, id: SurfaceId, f: F) -> Option<R> {
        self.inner.lock().get_mut(&id).map(f)
    }

    /// Drain the entire registry under the lock and run `f` for each
    /// `(id, state)`. Used by `Drop` impls that need to clean up GL /
    /// Vulkan resources owned by every entry without leaving the lock
    /// held while doing so. The map is empty after `drain`.
    pub fn drain<F: FnMut(SurfaceId, T)>(&self, mut f: F) {
        let drained: Vec<(SurfaceId, T)> = self.inner.lock().drain().collect();
        for (id, state) in drained {
            f(id, state);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Try to begin a read. Returns:
    /// - `Err(SurfaceNotFound)` if `id` isn't registered;
    /// - `Ok(None)` if a writer holds the surface (contention — caller
    ///   decides whether to escalate to [`AdapterError::WriteContended`]
    ///   or report try-acquire failure);
    /// - `Ok(Some(snapshot_value))` if the read is granted, with
    ///   `read_holders` already incremented under the lock.
    ///
    /// `snapshot` extracts whatever per-acquire data the adapter needs
    /// (timeline value, image handle, plane buffers, etc.) while the
    /// lock is held. If `snapshot` errors the counter is NOT
    /// incremented — atomic with the contention check.
    pub fn try_begin_read<R, F>(
        &self,
        id: SurfaceId,
        snapshot: F,
    ) -> Result<Option<R>, AdapterError>
    where
        F: FnOnce(&mut T) -> Result<R, AdapterError>,
    {
        let mut map = self.inner.lock();
        let state = map
            .get_mut(&id)
            .ok_or(AdapterError::SurfaceNotFound { surface_id: id })?;
        if state.write_held() {
            return Ok(None);
        }
        let r = snapshot(state)?;
        state.inc_read_holders();
        Ok(Some(r))
    }

    /// Try to begin a write. Same shape as
    /// [`try_begin_read`](Self::try_begin_read), but contention also
    /// triggers when any reader holds the surface. On success
    /// `write_held` is already set under the lock.
    pub fn try_begin_write<R, F>(
        &self,
        id: SurfaceId,
        snapshot: F,
    ) -> Result<Option<R>, AdapterError>
    where
        F: FnOnce(&mut T) -> Result<R, AdapterError>,
    {
        let mut map = self.inner.lock();
        let state = map
            .get_mut(&id)
            .ok_or(AdapterError::SurfaceNotFound { surface_id: id })?;
        if state.write_held() || state.read_holders() > 0 {
            return Ok(None);
        }
        let r = snapshot(state)?;
        state.set_write_held(true);
        Ok(Some(r))
    }

    /// Decrement `read_holders` (saturating). Used on guard-drop and
    /// post-acquire-timeout / post-finalize-error paths. Silently
    /// no-ops if `id` raced an unregister.
    pub fn rollback_read(&self, id: SurfaceId) {
        if let Some(state) = self.inner.lock().get_mut(&id) {
            state.dec_read_holders();
        }
    }

    /// Clear `write_held`. Symmetric with
    /// [`rollback_read`](Self::rollback_read).
    pub fn rollback_write(&self, id: SurfaceId) {
        if let Some(state) = self.inner.lock().get_mut(&id) {
            state.set_write_held(false);
        }
    }

    /// Build a one-line description of who's currently holding the
    /// surface, for [`AdapterError::WriteContended`] error bodies.
    /// Returns `"unknown"` if the id isn't registered.
    pub fn describe_contention(&self, id: SurfaceId) -> String {
        match self.inner.lock().get(&id) {
            Some(s) if s.write_held() => "writer".to_string(),
            Some(s) => format!("{} reader(s)", s.read_holders()),
            None => "unknown".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `SurfaceRegistration` impl for behavioral tests.
    #[derive(Default)]
    struct TestState {
        write_held: bool,
        read_holders: u64,
    }

    impl SurfaceRegistration for TestState {
        fn write_held(&self) -> bool {
            self.write_held
        }
        fn read_holders(&self) -> u64 {
            self.read_holders
        }
        fn set_write_held(&mut self, held: bool) {
            self.write_held = held;
        }
        fn inc_read_holders(&mut self) {
            self.read_holders += 1;
        }
        fn dec_read_holders(&mut self) {
            self.read_holders = self.read_holders.saturating_sub(1);
        }
    }

    fn registry_with(id: SurfaceId) -> Registry<TestState> {
        let r = Registry::<TestState>::new();
        assert!(r.register(id, TestState::default()));
        r
    }

    #[test]
    fn unknown_surface_returns_not_found() {
        let r = Registry::<TestState>::new();
        let res = r.try_begin_read(42, |_| Ok(()));
        assert!(matches!(
            res,
            Err(AdapterError::SurfaceNotFound { surface_id: 42 })
        ));
    }

    #[test]
    fn read_plus_read_concurrent_allowed() {
        let r = registry_with(1);
        // First read.
        let snap1 = r.try_begin_read(1, |_| Ok(())).unwrap();
        assert!(snap1.is_some());
        // Second concurrent read — also granted.
        let snap2 = r.try_begin_read(1, |_| Ok(())).unwrap();
        assert!(snap2.is_some());
        r.with(1, |s| {
            assert_eq!(s.read_holders, 2);
            assert!(!s.write_held);
        })
        .unwrap();
    }

    #[test]
    fn read_blocks_write() {
        let r = registry_with(1);
        let _read = r.try_begin_read(1, |_| Ok(())).unwrap().unwrap();
        let write = r.try_begin_write(1, |_| Ok(())).unwrap();
        assert!(write.is_none(), "write must be contended by reader");
        // State was not mutated.
        r.with(1, |s| {
            assert_eq!(s.read_holders, 1);
            assert!(!s.write_held);
        })
        .unwrap();
    }

    #[test]
    fn write_blocks_read() {
        let r = registry_with(1);
        let _write = r.try_begin_write(1, |_| Ok(())).unwrap().unwrap();
        let read = r.try_begin_read(1, |_| Ok(())).unwrap();
        assert!(read.is_none(), "read must be contended by writer");
        r.with(1, |s| {
            assert_eq!(s.read_holders, 0);
            assert!(s.write_held);
        })
        .unwrap();
    }

    #[test]
    fn write_blocks_write() {
        let r = registry_with(1);
        let _w1 = r.try_begin_write(1, |_| Ok(())).unwrap().unwrap();
        let w2 = r.try_begin_write(1, |_| Ok(())).unwrap();
        assert!(w2.is_none(), "second writer must be contended");
    }

    #[test]
    fn rollback_decrements_counters() {
        let r = registry_with(1);
        r.try_begin_read(1, |_| Ok(())).unwrap().unwrap();
        r.try_begin_read(1, |_| Ok(())).unwrap().unwrap();
        r.with(1, |s| assert_eq!(s.read_holders, 2)).unwrap();
        r.rollback_read(1);
        r.with(1, |s| assert_eq!(s.read_holders, 1)).unwrap();
        r.rollback_read(1);
        r.with(1, |s| assert_eq!(s.read_holders, 0)).unwrap();
    }

    #[test]
    fn rollback_read_is_saturating() {
        let r = registry_with(1);
        // No reader; rollback must not underflow.
        r.rollback_read(1);
        r.with(1, |s| assert_eq!(s.read_holders, 0)).unwrap();
    }

    #[test]
    fn rollback_write_clears_flag() {
        let r = registry_with(1);
        r.try_begin_write(1, |_| Ok(())).unwrap().unwrap();
        r.with(1, |s| assert!(s.write_held)).unwrap();
        r.rollback_write(1);
        r.with(1, |s| assert!(!s.write_held)).unwrap();
    }

    #[test]
    fn rollback_unknown_surface_no_ops() {
        let r = Registry::<TestState>::new();
        // Must not panic on unknown id.
        r.rollback_read(99);
        r.rollback_write(99);
    }

    #[test]
    fn snapshot_error_does_not_mutate_counters() {
        let r = registry_with(1);
        let res = r.try_begin_read::<(), _>(1, |_| {
            Err(AdapterError::SurfaceNotFound { surface_id: 1 })
        });
        assert!(matches!(
            res,
            Err(AdapterError::SurfaceNotFound { surface_id: 1 })
        ));
        r.with(1, |s| {
            assert_eq!(s.read_holders, 0);
            assert!(!s.write_held);
        })
        .unwrap();
    }

    #[test]
    fn snapshot_runs_under_lock_with_mut_state() {
        // The closure receives `&mut T`, so adapters can do per-acquire
        // state edits (e.g. record last_acquire_value) atomic with the
        // contention check. Use a side-channel — bumping write_held in
        // a read closure would be wrong; this test just confirms the
        // snapshot return value is plumbed through.
        let r = registry_with(1);
        let token = r
            .try_begin_read(1, |_state| Ok(42_u64))
            .unwrap()
            .unwrap();
        assert_eq!(token, 42);
        r.with(1, |s| assert_eq!(s.read_holders, 1)).unwrap();
    }

    #[test]
    fn describe_contention_reports_holders() {
        let r = registry_with(1);
        assert_eq!(r.describe_contention(1), "0 reader(s)");
        r.try_begin_read(1, |_| Ok(())).unwrap().unwrap();
        r.try_begin_read(1, |_| Ok(())).unwrap().unwrap();
        assert_eq!(r.describe_contention(1), "2 reader(s)");
        r.rollback_read(1);
        r.rollback_read(1);
        r.try_begin_write(1, |_| Ok(())).unwrap().unwrap();
        assert_eq!(r.describe_contention(1), "writer");
        assert_eq!(r.describe_contention(99), "unknown");
    }

    #[test]
    fn unregister_returns_state_and_removes() {
        let r = registry_with(1);
        assert_eq!(r.len(), 1);
        let taken = r.unregister(1);
        assert!(taken.is_some());
        assert_eq!(r.len(), 0);
        assert!(r.unregister(1).is_none());
    }

    #[test]
    fn duplicate_register_rejected() {
        let r = Registry::<TestState>::new();
        assert!(r.register(7, TestState::default()));
        assert!(!r.register(7, TestState::default()));
    }

    #[test]
    fn drain_visits_every_entry_and_empties() {
        let r = Registry::<TestState>::new();
        r.register(1, TestState::default());
        r.register(2, TestState::default());
        r.register(3, TestState::default());
        let mut seen = Vec::new();
        r.drain(|id, _| seen.push(id));
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3]);
        assert!(r.is_empty());
    }
}
