// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Whether an encoder may mint its session: always at first, never again
//! after a mint failed for good, and, while the GPU's driver holds as many
//! video sessions as it allows on the machine, once a retry interval after
//! each refusal, so the encoder starts once another session ends.

use std::time::{Duration, Instant};

/// How long after a mint the session limit refused the next one is tried.
pub(crate) const ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
enum EncodeSessionMintState {
    #[default]
    MayMint,
    RefusedAtTheSessionLimit {
        next_mint_not_before: Instant,
        mints_refused: u64,
    },
    FailedForGood,
}

/// The mint state of one encoder.
#[derive(Debug, Default)]
pub(crate) struct EncodeSessionMintGate {
    state: EncodeSessionMintState,
}

impl EncodeSessionMintGate {
    /// Whether a mint may be tried at `now`.
    pub(crate) fn a_mint_may_be_tried_at(&self, now: Instant) -> bool {
        match self.state {
            EncodeSessionMintState::MayMint => true,
            EncodeSessionMintState::RefusedAtTheSessionLimit {
                next_mint_not_before,
                ..
            } => now >= next_mint_not_before,
            EncodeSessionMintState::FailedForGood => false,
        }
    }

    /// Record that the session limit refused a mint at `now`, answering how
    /// many mints in a row it has refused.
    pub(crate) fn refused_at_the_session_limit_at(&mut self, now: Instant) -> u64 {
        let mints_refused = match self.state {
            EncodeSessionMintState::RefusedAtTheSessionLimit { mints_refused, .. } => {
                mints_refused + 1
            }
            EncodeSessionMintState::MayMint | EncodeSessionMintState::FailedForGood => 1,
        };
        self.state = EncodeSessionMintState::RefusedAtTheSessionLimit {
            next_mint_not_before: now + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL,
            mints_refused,
        };
        mints_refused
    }

    /// Record a mint that failed for a reason a later mint would meet again.
    pub(crate) fn failed_for_good(&mut self) {
        self.state = EncodeSessionMintState::FailedForGood;
    }

    /// Record a minted session, answering how many mints the session limit
    /// refused before it.
    pub(crate) fn minted(&mut self) -> u64 {
        let mints_refused = match self.state {
            EncodeSessionMintState::RefusedAtTheSessionLimit { mints_refused, .. } => mints_refused,
            EncodeSessionMintState::MayMint | EncodeSessionMintState::FailedForGood => 0,
        };
        self.state = EncodeSessionMintState::MayMint;
        mints_refused
    }
}
