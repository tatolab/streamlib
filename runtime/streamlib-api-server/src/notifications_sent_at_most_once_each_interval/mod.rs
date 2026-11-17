// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Notifications a control connection sends at most once each interval for one key: the first
//! is sent at once, those heard before the interval ends are held, the newest replacing the one
//! held, and the one held is sent as the interval ends, so the last is always sent.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How often one input's value change is sent to a connection subscribed to its stream's input
/// value changes.
pub(crate) const ONE_INPUT_VALUE_CHANGE_SENT_AT_MOST_EVERY: Duration = Duration::from_millis(33);

/// How often a connection that follows a stream's graph, and has not subscribed to its input
/// value changes, is told `edited` for a value change.
pub(crate) const A_VALUE_CHANGE_TOLD_AS_EDITED_AT_MOST_EVERY: Duration = Duration::from_millis(500);

/// The notifications of one connection sent at most once each `interval` for each key.
pub(crate) struct NotificationsSentAtMostOnceEachInterval<Key, Notification> {
    interval: Duration,
    last_sent_at: BTreeMap<Key, Instant>,
    held: BTreeMap<Key, Notification>,
}

impl<Key: Ord + Clone, Notification> NotificationsSentAtMostOnceEachInterval<Key, Notification> {
    /// None sent yet, each key sent at most once each `interval`.
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_sent_at: BTreeMap::new(),
            held: BTreeMap::new(),
        }
    }

    /// `notification`, heard for `key` at `now`, answered when it is sent at once; held, in place
    /// of any held for `key` before it, while `key`'s interval since its last send runs.
    pub(crate) fn heard(
        &mut self,
        key: Key,
        notification: Notification,
        now: Instant,
    ) -> Option<Notification> {
        self.forget_each_key_not_held_whose_interval_ran(now);
        let interval_runs = self
            .last_sent_at
            .get(&key)
            .is_some_and(|last_sent_at| now < *last_sent_at + self.interval);
        if interval_runs {
            self.held.insert(key, notification);
            return None;
        }
        self.held.remove(&key);
        self.last_sent_at.insert(key, now);
        Some(notification)
    }

    /// When the next held notification is due; `None` when none is held.
    pub(crate) fn next_due_at(&self) -> Option<Instant> {
        self.held
            .keys()
            .filter_map(|key| self.last_sent_at.get(key))
            .map(|last_sent_at| *last_sent_at + self.interval)
            .min()
    }

    /// Each held notification due at `now`, taken to be sent; the keys no longer held whose
    /// interval has run are forgotten.
    pub(crate) fn take_the_due(&mut self, now: Instant) -> Vec<Notification> {
        let interval = self.interval;
        let due: Vec<Key> = self
            .held
            .keys()
            .filter(|key| {
                self.last_sent_at
                    .get(*key)
                    .is_none_or(|last_sent_at| *last_sent_at + interval <= now)
            })
            .cloned()
            .collect();
        let mut taken = Vec::with_capacity(due.len());
        for key in due {
            if let Some(notification) = self.held.remove(&key) {
                self.last_sent_at.insert(key, now);
                taken.push(notification);
            }
        }
        self.forget_each_key_not_held_whose_interval_ran(now);
        taken
    }

    /// Forget when each key held no more was last sent, once its interval has run at `now`, so
    /// a key heard once is not remembered for the connection's life.
    fn forget_each_key_not_held_whose_interval_ran(&mut self, now: Instant) {
        let interval = self.interval;
        let held = &self.held;
        self.last_sent_at
            .retain(|key, last_sent_at| held.contains_key(key) || now < *last_sent_at + interval);
    }
}

#[cfg(test)]
mod tests;
