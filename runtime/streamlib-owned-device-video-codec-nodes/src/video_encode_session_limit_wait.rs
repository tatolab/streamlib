// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An encoder's wait for a video session while the GPU's driver holds as
//! many as it allows, counted across every process on the machine: the mint
//! is tried again at an interval, so the encoder starts once a session frees.

use std::time::{Duration, Instant};

/// How long after a mint the session limit refused the next mint is tried.
pub(crate) const ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// When the next mint may be tried, and how many mints in a row the session
/// limit refused.
#[derive(Debug, Default)]
pub(crate) struct WaitForAVideoSessionUnderTheLimit {
    next_mint_not_before: Option<Instant>,
    mints_refused: u64,
}

impl WaitForAVideoSessionUnderTheLimit {
    /// Whether a mint may be tried at `now`: always, unless the limit
    /// refused one less than the retry interval before.
    pub(crate) fn a_mint_may_be_tried_at(&self, now: Instant) -> bool {
        self.next_mint_not_before
            .is_none_or(|not_before| now >= not_before)
    }

    /// Record that the limit refused a mint at `now`, answering how many in
    /// a row it has refused.
    pub(crate) fn mint_refused_at(&mut self, now: Instant) -> u64 {
        self.next_mint_not_before = Some(now + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL);
        self.mints_refused += 1;
        self.mints_refused
    }

    /// Record that a mint succeeded, answering how many mints the limit
    /// refused before it.
    pub(crate) fn mint_succeeded(&mut self) -> u64 {
        self.next_mint_not_before = None;
        std::mem::take(&mut self.mints_refused)
    }
}
