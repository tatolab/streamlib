// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! When a node's host asks for the links its far side refused to be wired again: at once when a
//! new helper process starts in place of one that died, and while a far side that refused one
//! keeps running, soon after the refusal and then less often while one keeps being refused, so
//! a cause that passes is met quickly and one that stays costs a compile now and then. The
//! waits are the restart waits of a child process.

use std::time::Instant;

use crate::core::RuntimeContext;
use crate::core::child_process_restart_backoff::DoublingWait;
use crate::core::graph::NodeRunId;

/// The retries of one node's links a running far side refused, fed what the far side holds at
/// each turn of the node's host loop.
#[derive(Debug, Default)]
pub(super) struct RetriesOfTheLinksARunningFarSideRefused {
    wait: DoublingWait,
    next_retry_at: Option<Instant>,
}

impl RetriesOfTheLinksARunningFarSideRefused {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Whether to ask now, `holds_a_refused_link` saying whether the far side holds a link it
    /// refused at `now`. A far side holding none starts the waits over.
    pub(super) fn a_retry_is_due(&mut self, holds_a_refused_link: bool, now: Instant) -> bool {
        if !holds_a_refused_link {
            *self = Self::new();
            return false;
        }
        match self.next_retry_at {
            None => {
                self.next_retry_at = Some(now + self.wait.take());
                false
            }
            Some(retry_at) if now >= retry_at => {
                self.next_retry_at = Some(now + self.wait.take());
                true
            }
            Some(_) => false,
        }
    }
}

/// Ask the stream of `runtime_ctx` to wire again each link of node `id` an end refused, logging
/// a request the stream did not take.
pub(super) fn ask_for_each_link_an_end_refused_to_be_wired_again(
    id: &NodeRunId,
    runtime_ctx: &RuntimeContext,
) {
    if let Err(refusal) = runtime_ctx
        .runtime()
        .request_each_link_of_a_node_an_end_refused_be_wired_again(id)
    {
        tracing::warn!(
            "[{id}] the links its far side refused could not be asked to be wired again: \
             {refusal}"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::core::child_process_restart_backoff::{
        FIRST_CHILD_PROCESS_RESTART_WAIT, LONGEST_CHILD_PROCESS_RESTART_WAIT,
    };

    /// The first refusal seen waits its first wait, each retry that leaves a link refused waits
    /// twice as long up to the longest, and a far side holding none starts the waits over.
    ///
    /// Mental-revert: retrying at each turn of the host loop compiles the stream ten times a
    /// second for a link that cannot be wired; never starting over leaves a later refusal
    /// waiting the longest wait.
    #[test]
    fn a_refused_link_is_retried_after_waits_that_double_and_start_over_once_none_is_refused() {
        let mut retries = RetriesOfTheLinksARunningFarSideRefused::new();
        let seen_at = Instant::now();

        assert!(!retries.a_retry_is_due(true, seen_at));
        assert!(!retries.a_retry_is_due(true, seen_at + Duration::from_millis(999)));
        assert!(retries.a_retry_is_due(true, seen_at + Duration::from_secs(1)));
        assert!(!retries.a_retry_is_due(true, seen_at + Duration::from_secs(2)));
        assert!(retries.a_retry_is_due(true, seen_at + Duration::from_secs(3)));

        let mut retried_at = seen_at + Duration::from_secs(3);
        for _ in 0..8 {
            retried_at += LONGEST_CHILD_PROCESS_RESTART_WAIT;
            assert!(retries.a_retry_is_due(true, retried_at));
        }
        assert!(!retries.a_retry_is_due(
            true,
            retried_at + LONGEST_CHILD_PROCESS_RESTART_WAIT - Duration::from_millis(1)
        ));

        assert!(!retries.a_retry_is_due(false, retried_at));
        assert!(!retries.a_retry_is_due(true, retried_at));
        assert!(retries.a_retry_is_due(true, retried_at + FIRST_CHILD_PROCESS_RESTART_WAIT));
    }
}
