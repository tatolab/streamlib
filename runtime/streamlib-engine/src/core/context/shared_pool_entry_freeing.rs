// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! When an entry of one of the runtime's shared pools — the texture pool, the
//! pixel-buffer rings — may be freed, once the pool has judged it unheld.
//!
//! Each pool decides "unheld" by its own holders. What both share is kept
//! here: when the entry was first seen unheld and what had been submitted to
//! the GPU by then, which stream last acquired it, and whether its owner went
//! away so it skips the idle window.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::core::rhi::GpuQueueSubmissionsSnapshot;
use crate::core::runtime::LoadedStreamTag;

/// How long a shared pool entry sits unheld before it is freed. A tuning
/// constant, not a contract.
pub(crate) const SHARED_POOL_ENTRY_IDLE_WINDOW_BEFORE_IT_IS_FREED: Duration =
    Duration::from_secs(5);

/// The longest a stopping stream waits for the GPU work naming the shared pool
/// entries it last acquired; entries whose work outlasts it are freed later.
pub(crate) const LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK: Duration =
    Duration::from_secs(1);

/// The idle window one shared pool frees by:
/// [`SHARED_POOL_ENTRY_IDLE_WINDOW_BEFORE_IT_IS_FREED`], or what a test set.
pub(crate) struct SharedPoolEntryIdleWindow {
    idle_window_in_nanoseconds: AtomicU64,
}

impl Default for SharedPoolEntryIdleWindow {
    fn default() -> Self {
        Self {
            idle_window_in_nanoseconds: AtomicU64::new(
                SHARED_POOL_ENTRY_IDLE_WINDOW_BEFORE_IT_IS_FREED.as_nanos() as u64,
            ),
        }
    }
}

impl SharedPoolEntryIdleWindow {
    /// The window an unheld entry must outlast before it is freed.
    pub(crate) fn duration(&self) -> Duration {
        Duration::from_nanos(self.idle_window_in_nanoseconds.load(Ordering::Relaxed))
    }

    /// Replace the window for one test's pool.
    #[cfg(test)]
    pub(crate) fn set_for_a_test(&self, idle_window: Duration) {
        self.idle_window_in_nanoseconds
            .store(idle_window.as_nanos() as u64, Ordering::Relaxed);
    }
}

/// The moment an entry was first seen unheld, and what had been submitted to
/// the GPU by then. Every submission naming the entry was made before its last
/// holder let go, so this snapshot covers all of them.
#[derive(Clone, Copy, Debug)]
struct SharedPoolEntryFirstObservedUnheld {
    observed_at: Instant,
    queue_submissions_made_by_then: GpuQueueSubmissionsSnapshot,
}

/// One shared pool entry's freeing bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct SharedPoolEntryFreeingRecord {
    /// `None` for an entry the engine's own context acquired.
    stream_that_last_acquired_the_entry: Option<LoadedStreamTag>,
    first_observed_unheld: Option<SharedPoolEntryFirstObservedUnheld>,
    /// Set when the entry's stream stopped or its node's pool was torn down
    /// while it was held: once unheld it is freed without the idle window.
    freed_as_soon_as_unheld: bool,
}

impl SharedPoolEntryFreeingRecord {
    /// The record of an entry just handed out to `acquiring_stream`.
    pub(crate) fn handed_out_to(acquiring_stream: Option<LoadedStreamTag>) -> Self {
        Self {
            stream_that_last_acquired_the_entry: acquiring_stream,
            ..Self::default()
        }
    }

    /// The entry was handed out again: its idle time and any owner-gone flag
    /// end, and `acquiring_stream` now owns it.
    pub(crate) fn note_handed_out_again_to(&mut self, acquiring_stream: Option<LoadedStreamTag>) {
        *self = Self::handed_out_to(acquiring_stream);
    }

    /// The pool saw the entry held: whatever idle time it had ends.
    pub(crate) fn note_observed_held(&mut self) {
        self.first_observed_unheld = None;
    }

    /// The pool saw the entry unheld at `now`. The first such sighting since it
    /// was last held records `now` and the GPU snapshot `snapshot_now` takes;
    /// later sightings keep both.
    pub(crate) fn note_observed_unheld(
        &mut self,
        now: Instant,
        snapshot_now: impl FnOnce() -> GpuQueueSubmissionsSnapshot,
    ) {
        if self.first_observed_unheld.is_none() {
            self.first_observed_unheld = Some(SharedPoolEntryFirstObservedUnheld {
                observed_at: now,
                queue_submissions_made_by_then: snapshot_now(),
            });
        }
    }

    /// Free the entry as soon as it is unheld, skipping the idle window.
    pub(crate) fn flag_freed_as_soon_as_unheld(&mut self) {
        self.freed_as_soon_as_unheld = true;
    }

    /// Whether `stream` is the stream that last acquired the entry.
    pub(crate) fn was_last_acquired_by(&self, stream: LoadedStreamTag) -> bool {
        self.stream_that_last_acquired_the_entry == Some(stream)
    }

    /// The GPU work that must complete before the entry is freed; `None` until
    /// the entry has been seen unheld.
    pub(crate) fn queue_submissions_made_before_it_was_unheld(
        &self,
    ) -> Option<GpuQueueSubmissionsSnapshot> {
        self.first_observed_unheld
            .map(|first_observed_unheld| first_observed_unheld.queue_submissions_made_by_then)
    }

    /// Whether the entry may be freed at `now`: it has been seen unheld, has
    /// been so for `idle_window` or was flagged to skip it, and every
    /// submission made before it was unheld has completed.
    pub(crate) fn may_be_freed_now(
        &self,
        now: Instant,
        idle_window: Duration,
        every_queue_submission_in_snapshot_has_completed: impl FnOnce(
            &GpuQueueSubmissionsSnapshot,
        ) -> bool,
    ) -> bool {
        let Some(first_observed_unheld) = self.first_observed_unheld else {
            return false;
        };
        let idle_long_enough = self.freed_as_soon_as_unheld
            || now.saturating_duration_since(first_observed_unheld.observed_at) >= idle_window;
        idle_long_enough
            && every_queue_submission_in_snapshot_has_completed(
                &first_observed_unheld.queue_submissions_made_by_then,
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    const IDLE_WINDOW: Duration = Duration::from_secs(5);

    fn the_gpu_work_has_completed(_: &GpuQueueSubmissionsSnapshot) -> bool {
        true
    }

    fn the_gpu_work_is_still_running(_: &GpuQueueSubmissionsSnapshot) -> bool {
        false
    }

    #[test]
    fn an_entry_never_seen_unheld_is_never_freed() {
        let record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        let far_later = Instant::now() + IDLE_WINDOW * 100;
        assert!(!record.may_be_freed_now(far_later, IDLE_WINDOW, the_gpu_work_has_completed));
    }

    #[test]
    fn an_unheld_entry_is_freed_once_it_outlasts_the_idle_window_and_not_before() {
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        record.note_observed_unheld(first_seen_unheld, GpuQueueSubmissionsSnapshot::default);

        let just_inside = first_seen_unheld + IDLE_WINDOW - Duration::from_millis(1);
        assert!(!record.may_be_freed_now(just_inside, IDLE_WINDOW, the_gpu_work_has_completed));
        let at_the_window = first_seen_unheld + IDLE_WINDOW;
        assert!(record.may_be_freed_now(at_the_window, IDLE_WINDOW, the_gpu_work_has_completed));
    }

    #[test]
    fn an_idle_entry_whose_gpu_work_is_still_running_is_not_freed() {
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        record.note_observed_unheld(first_seen_unheld, GpuQueueSubmissionsSnapshot::default);

        let far_later = first_seen_unheld + IDLE_WINDOW * 100;
        assert!(!record.may_be_freed_now(far_later, IDLE_WINDOW, the_gpu_work_is_still_running));
    }

    #[test]
    fn the_gpu_snapshot_is_taken_at_the_first_unheld_sighting_only() {
        let snapshots_taken = Cell::new(0);
        let take_snapshot = || {
            snapshots_taken.set(snapshots_taken.get() + 1);
            GpuQueueSubmissionsSnapshot::default()
        };
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        record.note_observed_unheld(first_seen_unheld, take_snapshot);
        record.note_observed_unheld(first_seen_unheld + IDLE_WINDOW, take_snapshot);
        assert_eq!(snapshots_taken.get(), 1);
        assert!(
            record
                .queue_submissions_made_before_it_was_unheld()
                .is_some()
        );

        let at_the_first_sighting_plus_the_window = first_seen_unheld + IDLE_WINDOW;
        assert!(
            record.may_be_freed_now(
                at_the_first_sighting_plus_the_window,
                IDLE_WINDOW,
                the_gpu_work_has_completed
            ),
            "a later sighting moved the idle clock"
        );
    }

    #[test]
    fn an_entry_seen_held_again_restarts_its_idle_window() {
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        record.note_observed_unheld(first_seen_unheld, GpuQueueSubmissionsSnapshot::default);
        record.note_observed_held();
        assert!(
            record
                .queue_submissions_made_before_it_was_unheld()
                .is_none()
        );

        let seen_unheld_again = first_seen_unheld + IDLE_WINDOW;
        record.note_observed_unheld(seen_unheld_again, GpuQueueSubmissionsSnapshot::default);
        assert!(!record.may_be_freed_now(
            seen_unheld_again,
            IDLE_WINDOW,
            the_gpu_work_has_completed
        ));
        assert!(record.may_be_freed_now(
            seen_unheld_again + IDLE_WINDOW,
            IDLE_WINDOW,
            the_gpu_work_has_completed
        ));
    }

    #[test]
    fn an_entry_flagged_freed_as_soon_as_unheld_skips_the_idle_window_but_not_its_gpu_work() {
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        record.flag_freed_as_soon_as_unheld();
        assert!(
            !record.may_be_freed_now(first_seen_unheld, IDLE_WINDOW, the_gpu_work_has_completed),
            "a flagged entry is still freed only once it is seen unheld"
        );

        record.note_observed_unheld(first_seen_unheld, GpuQueueSubmissionsSnapshot::default);
        assert!(record.may_be_freed_now(
            first_seen_unheld,
            IDLE_WINDOW,
            the_gpu_work_has_completed
        ));
        assert!(!record.may_be_freed_now(
            first_seen_unheld,
            IDLE_WINDOW,
            the_gpu_work_is_still_running
        ));
    }

    #[test]
    fn handing_an_entry_out_again_ends_its_idle_time_and_its_flag_and_records_the_new_stream() {
        let first_stream = LoadedStreamTag::numbered_for_a_test(1);
        let second_stream = LoadedStreamTag::numbered_for_a_test(2);
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(Some(first_stream));
        assert!(record.was_last_acquired_by(first_stream));
        record.flag_freed_as_soon_as_unheld();
        record.note_observed_unheld(first_seen_unheld, GpuQueueSubmissionsSnapshot::default);

        record.note_handed_out_again_to(Some(second_stream));
        assert!(record.was_last_acquired_by(second_stream));
        assert!(!record.was_last_acquired_by(first_stream));
        assert!(
            record
                .queue_submissions_made_before_it_was_unheld()
                .is_none()
        );

        let seen_unheld_again = first_seen_unheld + IDLE_WINDOW;
        record.note_observed_unheld(seen_unheld_again, GpuQueueSubmissionsSnapshot::default);
        assert!(
            !record.may_be_freed_now(seen_unheld_again, IDLE_WINDOW, the_gpu_work_has_completed),
            "the old owner's flag survived the entry being handed out again"
        );
    }

    #[test]
    fn an_entry_the_engines_own_context_acquired_belongs_to_no_stream() {
        let record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        assert!(!record.was_last_acquired_by(LoadedStreamTag::numbered_for_a_test(1)));
    }

    #[test]
    fn a_pools_idle_window_starts_at_the_tuning_constant_and_a_test_can_set_it() {
        let idle_window = SharedPoolEntryIdleWindow::default();
        assert_eq!(
            idle_window.duration(),
            SHARED_POOL_ENTRY_IDLE_WINDOW_BEFORE_IT_IS_FREED
        );
        idle_window.set_for_a_test(Duration::ZERO);
        assert_eq!(idle_window.duration(), Duration::ZERO);
    }
}
