// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! When an entry of one of the runtime's shared pools — the texture pool, the
//! pixel-buffer rings — is freed, once the pool has judged it unheld.
//!
//! Each pool decides "unheld" by its own holders. What both share is kept
//! here: when the entry was first seen unheld, which stream last acquired it,
//! whether its owner went away so it skips the idle window, which entries one
//! acquire or release looks at, and the wait between taking an entry out of
//! its pool and destroying it — until nothing else references its memory and
//! the GPU work submitted by then has completed.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::core::rhi::{GpuDevice, GpuQueueSubmissionsSnapshot};
use crate::core::runtime::LoadedStreamTag;

/// How long a shared pool entry sits unheld before it is freed. A tuning
/// constant, not a contract.
pub(crate) const SHARED_POOL_ENTRY_IDLE_WINDOW_BEFORE_IT_IS_FREED: Duration =
    Duration::from_secs(5);

/// The longest a stopping stream waits for the GPU work naming the shared pool
/// entries it frees; entries whose work outlasts it are destroyed later.
pub(crate) const LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK: Duration =
    Duration::from_secs(1);

/// The longest a stopping stream waits for the checkout leases naming the
/// shared pool entries it last acquired to clear. Its helpers are reaped by
/// then, and the surface-share service releases a closed connection's leases
/// on its own thread; a lease outlasting the wait is a holder outside the
/// stream, and its entry is freed by a later acquire or release.
pub(crate) const LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES:
    Duration = Duration::from_secs(1);

/// The longest a shared pool being dropped waits for the GPU work naming the
/// entries it took out and has not destroyed yet.
const LONGEST_A_DROPPED_POOL_WAITS_FOR_THE_GPU_WORK_OF_ITS_ENTRIES_AWAITING_DESTRUCTION: Duration =
    Duration::from_secs(1);

/// How many entries of one shared pool a single acquire or release observes,
/// resuming where the previous one stopped. A tuning constant: it bounds the
/// per-frame cost of the scan, and a pool of `n` entries sees each one every
/// `n / SHARED_POOL_ENTRIES_OBSERVED_PER_ACQUIRE_OR_RELEASE` calls.
pub(crate) const SHARED_POOL_ENTRIES_OBSERVED_PER_ACQUIRE_OR_RELEASE: usize = 8;

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

/// One shared pool entry's freeing bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct SharedPoolEntryFreeingRecord {
    /// `None` for an entry the engine's own context acquired.
    stream_that_last_acquired_the_entry: Option<LoadedStreamTag>,
    first_observed_unheld_at: Option<Instant>,
    /// Set when the entry's stream stopped or its owner was torn down: once
    /// unheld it is freed without the idle window.
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

    /// Free the entry as soon as it is unheld, skipping the idle window.
    pub(crate) fn flag_freed_as_soon_as_unheld(&mut self) {
        self.freed_as_soon_as_unheld = true;
    }

    /// Whether `stream` is the stream that last acquired the entry.
    pub(crate) fn was_last_acquired_by(&self, stream: LoadedStreamTag) -> bool {
        self.stream_that_last_acquired_the_entry == Some(stream)
    }
}

/// One pass of a pool over its entries, at one moment and one idle window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SharedPoolEntryFreeingScan {
    now: Instant,
    idle_window: Duration,
}

impl SharedPoolEntryFreeingScan {
    /// A scan at this moment, by the pool's `idle_window`.
    pub(crate) fn starting_now(idle_window: &SharedPoolEntryIdleWindow) -> Self {
        Self {
            now: Instant::now(),
            idle_window: idle_window.duration(),
        }
    }

    /// Note on `record` whether this scan found its entry unheld, answering
    /// whether the entry is due to be freed: unheld, and so since a sighting
    /// at least the idle window ago or flagged to skip it.
    pub(crate) fn observe(
        &self,
        record: &mut SharedPoolEntryFreeingRecord,
        is_unheld: bool,
    ) -> bool {
        if !is_unheld {
            record.first_observed_unheld_at = None;
            return false;
        }
        let first_observed_unheld_at = *record.first_observed_unheld_at.get_or_insert(self.now);
        record.freed_as_soon_as_unheld
            || self.now.saturating_duration_since(first_observed_unheld_at) >= self.idle_window
    }
}

/// Where a pool's next bounded observation of its entries starts.
#[derive(Default)]
pub(crate) struct SharedPoolEntryObservationCursor {
    position_of_the_next_entry_to_observe: AtomicUsize,
}

impl SharedPoolEntryObservationCursor {
    /// The positions, across a pool of `entry_count` entries, the next
    /// acquire or release observes: at most
    /// [`SHARED_POOL_ENTRIES_OBSERVED_PER_ACQUIRE_OR_RELEASE`], resuming where
    /// the previous window ended and starting over past the last entry.
    pub(crate) fn next_window_over(&self, entry_count: usize) -> SharedPoolEntryObservationWindow {
        if entry_count == 0 {
            return SharedPoolEntryObservationWindow::every_entry_of(0);
        }
        let start = self
            .position_of_the_next_entry_to_observe
            .load(Ordering::Relaxed)
            % entry_count;
        let end = (start + SHARED_POOL_ENTRIES_OBSERVED_PER_ACQUIRE_OR_RELEASE).min(entry_count);
        self.position_of_the_next_entry_to_observe
            .store(if end == entry_count { 0 } else { end }, Ordering::Relaxed);
        SharedPoolEntryObservationWindow {
            positions_across_the_pool: start..end,
        }
    }
}

/// The positions across a pool — its groups (rings, buckets) laid end to end
/// in iteration order — that one scan observes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SharedPoolEntryObservationWindow {
    positions_across_the_pool: Range<usize>,
}

impl SharedPoolEntryObservationWindow {
    /// Every entry of a pool of `entry_count`.
    pub(crate) fn every_entry_of(entry_count: usize) -> Self {
        Self {
            positions_across_the_pool: 0..entry_count,
        }
    }

    /// The indices within one group, whose first entry sits at
    /// `position_of_the_groups_first_entry` and which holds
    /// `entry_count_of_the_group`, that the window covers.
    pub(crate) fn indices_within_the_group(
        &self,
        position_of_the_groups_first_entry: usize,
        entry_count_of_the_group: usize,
    ) -> Range<usize> {
        let position_past_the_groups_last_entry =
            position_of_the_groups_first_entry + entry_count_of_the_group;
        let start = self.positions_across_the_pool.start.clamp(
            position_of_the_groups_first_entry,
            position_past_the_groups_last_entry,
        );
        let end = self
            .positions_across_the_pool
            .end
            .clamp(start, position_past_the_groups_last_entry);
        if start == end {
            return 0..0;
        }
        (start - position_of_the_groups_first_entry)..(end - position_of_the_groups_first_entry)
    }
}

/// A shared pool entry taken out of its pool, whose memory goes when it drops.
pub(crate) trait SharedPoolEntryTakenOutOfItsPool {
    /// Whether anything besides the taken-out entry still references its
    /// memory.
    fn is_referenced_beyond_the_taken_out_entry(&self) -> bool;

    /// Log, at debug, that the entry is being destroyed.
    fn note_its_destruction(&self);
}

struct SharedPoolEntryAwaitingDestruction<Entry> {
    entry: Entry,
    /// What this process had submitted to the GPU once nothing but the
    /// taken-out entry referenced its memory; `None` until then.
    queue_submissions_made_once_nothing_else_referenced_it: Option<GpuQueueSubmissionsSnapshot>,
}

impl<Entry: SharedPoolEntryTakenOutOfItsPool> SharedPoolEntryAwaitingDestruction<Entry> {
    /// The GPU work that must complete before the entry is destroyed; `None`
    /// while something besides it references its memory. Taken out of its
    /// pool, the entry gains no new holder, so every submission naming it was
    /// made before its last other holder let go, and the snapshot taken once
    /// none is left covers them all.
    fn queue_submissions_naming_it(
        &mut self,
        device: &GpuDevice,
    ) -> Option<GpuQueueSubmissionsSnapshot> {
        if self
            .queue_submissions_made_once_nothing_else_referenced_it
            .is_none()
        {
            if self.entry.is_referenced_beyond_the_taken_out_entry() {
                return None;
            }
            // The holder count above was read relaxed. This fence pairs with
            // the release decrement of the last other holder's drop, so the
            // snapshot sees every submission that holder made before it.
            std::sync::atomic::fence(Ordering::Acquire);
            self.queue_submissions_made_once_nothing_else_referenced_it =
                Some(device.snapshot_of_queue_submissions_made_so_far());
        }
        self.queue_submissions_made_once_nothing_else_referenced_it
    }
}

/// The GPU work naming every entry of `entries_awaiting_destruction` nothing
/// else references, as one snapshot.
fn queue_submissions_naming_every_unreferenced_entry<Entry: SharedPoolEntryTakenOutOfItsPool>(
    entries_awaiting_destruction: &mut [SharedPoolEntryAwaitingDestruction<Entry>],
    device: &GpuDevice,
) -> GpuQueueSubmissionsSnapshot {
    entries_awaiting_destruction
        .iter_mut()
        .filter_map(|awaiting| awaiting.queue_submissions_naming_it(device))
        .fold(
            GpuQueueSubmissionsSnapshot::default(),
            |covering_the_others, queue_submissions| {
                covering_the_others.covering_both(&queue_submissions)
            },
        )
}

/// The entries one shared pool has taken out, each destroyed once nothing
/// else references its memory and the GPU work naming it has completed.
pub(crate) struct SharedPoolEntriesAwaitingDestruction<Entry: SharedPoolEntryTakenOutOfItsPool> {
    entries_awaiting_destruction: Mutex<Vec<SharedPoolEntryAwaitingDestruction<Entry>>>,
    /// Names the pool in a log line.
    pool_label: &'static str,
    device: Arc<GpuDevice>,
}

impl<Entry: SharedPoolEntryTakenOutOfItsPool> SharedPoolEntriesAwaitingDestruction<Entry> {
    /// No entry awaiting destruction, for the pool `pool_label` names on
    /// `device`.
    pub(crate) fn new(pool_label: &'static str, device: Arc<GpuDevice>) -> Self {
        Self {
            entries_awaiting_destruction: Mutex::new(Vec::new()),
            pool_label,
            device,
        }
    }

    /// How many entries await destruction.
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.entries_awaiting_destruction.lock().len()
    }

    /// Hold `entries_taken_out` until each may be destroyed, then destroy at
    /// most one that may be destroyed now, without blocking: destroying can
    /// take milliseconds, and this runs on a producer's acquire or release.
    pub(crate) fn park_and_destroy_at_most_one_ready(
        &self,
        entries_taken_out: impl IntoIterator<Item = Entry>,
    ) {
        self.park(entries_taken_out);
        destroy(self.take_back_the_ones_ready_to_destroy(1));
    }

    /// Hold `entries_taken_out` until each may be destroyed, then destroy
    /// every entry that may be destroyed now, without blocking; answers how
    /// many were destroyed.
    pub(crate) fn park_and_destroy_every_one_ready(
        &self,
        entries_taken_out: impl IntoIterator<Item = Entry>,
    ) -> usize {
        self.park(entries_taken_out);
        destroy(self.take_back_the_ones_ready_to_destroy(usize::MAX))
    }

    /// Hold `entries_taken_out` until each may be destroyed, block at most
    /// `longest_wait` for the GPU work naming every entry nothing else
    /// references, then destroy every entry that may be destroyed; answers
    /// how many were destroyed.
    pub(crate) fn park_and_destroy_every_one_ready_waiting_at_most(
        &self,
        entries_taken_out: impl IntoIterator<Item = Entry>,
        longest_wait: Duration,
    ) -> usize {
        self.park(entries_taken_out);
        let queue_submissions_naming_them = queue_submissions_naming_every_unreferenced_entry(
            &mut self.entries_awaiting_destruction.lock(),
            &self.device,
        );
        match self.device.wait_for_every_queue_submission_in_snapshot(
            &queue_submissions_naming_them,
            longest_wait,
        ) {
            Ok(true) => {}
            outlasted_or_unreadable => tracing::warn!(
                "the GPU work naming the {} entries awaiting destruction did not complete within \
                 {longest_wait:?} ({outlasted_or_unreadable:?}); they are destroyed once it has",
                self.pool_label
            ),
        }
        self.park_and_destroy_every_one_ready([])
    }

    fn park(&self, entries_taken_out: impl IntoIterator<Item = Entry>) {
        self.entries_awaiting_destruction
            .lock()
            .extend(entries_taken_out.into_iter().map(|entry| {
                SharedPoolEntryAwaitingDestruction {
                    entry,
                    queue_submissions_made_once_nothing_else_referenced_it: None,
                }
            }));
    }

    /// Take back at most `most` entries that may be destroyed now, without
    /// blocking; the caller drops them outside this lock.
    fn take_back_the_ones_ready_to_destroy(&self, most: usize) -> Vec<Entry> {
        let mut entries_awaiting_destruction = self.entries_awaiting_destruction.lock();
        let mut indices_ready_to_destroy = Vec::new();
        for (index, awaiting) in entries_awaiting_destruction.iter_mut().enumerate() {
            if indices_ready_to_destroy.len() == most {
                break;
            }
            if awaiting
                .queue_submissions_naming_it(&self.device)
                .is_some_and(|queue_submissions| {
                    every_queue_submission_in_snapshot_has_completed(
                        &self.device,
                        &queue_submissions,
                        self.pool_label,
                    )
                })
            {
                indices_ready_to_destroy.push(index);
            }
        }
        indices_ready_to_destroy
            .into_iter()
            .rev()
            .map(|index| entries_awaiting_destruction.swap_remove(index).entry)
            .collect()
    }
}

impl<Entry: SharedPoolEntryTakenOutOfItsPool> Drop for SharedPoolEntriesAwaitingDestruction<Entry> {
    // Nothing destroys these entries later, so an entry whose GPU work the
    // bounded wait did not see complete is leaked rather than destroyed under
    // the GPU. One something else still references goes with that holder.
    fn drop(&mut self) {
        let mut entries_awaiting_destruction =
            std::mem::take(self.entries_awaiting_destruction.get_mut());
        if entries_awaiting_destruction.is_empty() {
            return;
        }
        let queue_submissions_naming_them = queue_submissions_naming_every_unreferenced_entry(
            &mut entries_awaiting_destruction,
            &self.device,
        );
        let their_gpu_work_completed = matches!(
            self.device.wait_for_every_queue_submission_in_snapshot(
                &queue_submissions_naming_them,
                LONGEST_A_DROPPED_POOL_WAITS_FOR_THE_GPU_WORK_OF_ITS_ENTRIES_AWAITING_DESTRUCTION,
            ),
            Ok(true)
        );
        let mut leaked_entry_count = 0;
        for awaiting in entries_awaiting_destruction {
            let is_destroyed_by_this_drop = awaiting
                .queue_submissions_made_once_nothing_else_referenced_it
                .is_some();
            if is_destroyed_by_this_drop && !their_gpu_work_completed {
                std::mem::forget(awaiting.entry);
                leaked_entry_count += 1;
            } else if is_destroyed_by_this_drop {
                awaiting.entry.note_its_destruction();
            }
        }
        if leaked_entry_count > 0 {
            tracing::warn!(
                "the {} was dropped with {leaked_entry_count} entries whose GPU work did not \
                 complete within {:?}; they are leaked rather than destroyed under the GPU",
                self.pool_label,
                LONGEST_A_DROPPED_POOL_WAITS_FOR_THE_GPU_WORK_OF_ITS_ENTRIES_AWAITING_DESTRUCTION
            );
        }
    }
}

/// Destroy `entries`, answering how many.
fn destroy<Entry: SharedPoolEntryTakenOutOfItsPool>(entries: Vec<Entry>) -> usize {
    let destroyed_entry_count = entries.len();
    for entry in entries {
        entry.note_its_destruction();
        drop(entry);
    }
    destroyed_entry_count
}

/// Whether every submission `snapshot` names has completed; an unreadable
/// answer is logged and counts as not yet, so nothing is destroyed on it.
fn every_queue_submission_in_snapshot_has_completed(
    device: &GpuDevice,
    snapshot: &GpuQueueSubmissionsSnapshot,
    pool_label: &str,
) -> bool {
    device
        .every_queue_submission_in_snapshot_has_completed(snapshot)
        .unwrap_or_else(|unreadable| {
            tracing::warn!(
                "the GPU's queue completion could not be read, so no {pool_label} entry is \
                 destroyed: {unreadable}"
            );
            false
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE_WINDOW: Duration = Duration::from_secs(5);

    fn a_scan_at(now: Instant) -> SharedPoolEntryFreeingScan {
        SharedPoolEntryFreeingScan {
            now,
            idle_window: IDLE_WINDOW,
        }
    }

    #[test]
    fn an_entry_seen_held_is_never_due_to_be_freed() {
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        let far_later = Instant::now() + IDLE_WINDOW * 100;
        assert!(!a_scan_at(far_later).observe(&mut record, false));
    }

    #[test]
    fn an_unheld_entry_is_due_once_it_outlasts_the_idle_window_and_not_before() {
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        assert!(!a_scan_at(first_seen_unheld).observe(&mut record, true));

        let just_inside = first_seen_unheld + IDLE_WINDOW - Duration::from_millis(1);
        assert!(!a_scan_at(just_inside).observe(&mut record, true));
        let at_the_window = first_seen_unheld + IDLE_WINDOW;
        assert!(
            a_scan_at(at_the_window).observe(&mut record, true),
            "a later sighting moved the idle clock"
        );
    }

    #[test]
    fn an_entry_seen_held_again_restarts_its_idle_window() {
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        a_scan_at(first_seen_unheld).observe(&mut record, true);
        a_scan_at(first_seen_unheld).observe(&mut record, false);

        let seen_unheld_again = first_seen_unheld + IDLE_WINDOW;
        assert!(!a_scan_at(seen_unheld_again).observe(&mut record, true));
        assert!(a_scan_at(seen_unheld_again + IDLE_WINDOW).observe(&mut record, true));
    }

    #[test]
    fn an_entry_flagged_freed_as_soon_as_unheld_skips_the_idle_window_but_not_its_holders() {
        let now = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(None);
        record.flag_freed_as_soon_as_unheld();
        assert!(!a_scan_at(now).observe(&mut record, false));
        assert!(a_scan_at(now).observe(&mut record, true));
    }

    #[test]
    fn handing_an_entry_out_again_ends_its_idle_time_and_its_flag_and_records_the_new_stream() {
        let first_stream = LoadedStreamTag::numbered_for_a_test(1);
        let second_stream = LoadedStreamTag::numbered_for_a_test(2);
        let first_seen_unheld = Instant::now();
        let mut record = SharedPoolEntryFreeingRecord::handed_out_to(Some(first_stream));
        assert!(record.was_last_acquired_by(first_stream));
        record.flag_freed_as_soon_as_unheld();
        a_scan_at(first_seen_unheld).observe(&mut record, true);

        record.note_handed_out_again_to(Some(second_stream));
        assert!(record.was_last_acquired_by(second_stream));
        assert!(!record.was_last_acquired_by(first_stream));
        assert!(
            !a_scan_at(first_seen_unheld + IDLE_WINDOW).observe(&mut record, true),
            "the old owner's flag or idle time survived the entry being handed out again"
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

    #[test]
    fn successive_observation_windows_cover_every_entry_a_bounded_few_at_a_time() {
        let cursor = SharedPoolEntryObservationCursor::default();
        let entry_count = SHARED_POOL_ENTRIES_OBSERVED_PER_ACQUIRE_OR_RELEASE * 2 + 3;
        let mut observed_count_per_entry = vec![0; entry_count];
        let mut window_count = 0;
        while observed_count_per_entry.contains(&0) {
            let window = cursor.next_window_over(entry_count);
            let observed = window.indices_within_the_group(0, entry_count);
            assert!(observed.len() <= SHARED_POOL_ENTRIES_OBSERVED_PER_ACQUIRE_OR_RELEASE);
            for position in observed {
                observed_count_per_entry[position] += 1;
            }
            window_count += 1;
        }
        assert_eq!(window_count, 3);
        assert!(observed_count_per_entry.iter().all(|&count| count == 1));
        assert_eq!(
            cursor.next_window_over(entry_count),
            SharedPoolEntryObservationWindow {
                positions_across_the_pool: 0..SHARED_POOL_ENTRIES_OBSERVED_PER_ACQUIRE_OR_RELEASE
            },
            "the cursor did not start over past the last entry"
        );
        assert!(
            cursor
                .next_window_over(0)
                .indices_within_the_group(0, 0)
                .is_empty()
        );
    }

    #[test]
    fn a_window_splits_across_the_groups_it_spans() {
        let window = SharedPoolEntryObservationWindow {
            positions_across_the_pool: 3..9,
        };
        assert_eq!(window.indices_within_the_group(0, 2), 0..0);
        assert_eq!(window.indices_within_the_group(2, 4), 1..4);
        assert_eq!(window.indices_within_the_group(6, 2), 0..2);
        assert_eq!(window.indices_within_the_group(8, 5), 0..1);
        assert_eq!(window.indices_within_the_group(13, 4), 0..0);
    }
}
