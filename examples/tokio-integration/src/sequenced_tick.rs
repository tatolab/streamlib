// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The bag on this app's one link.

use serde::{Deserialize, Serialize};

/// One tick: where it sits in the producer's sequence, and the
/// machine-monotonic instant it was stamped at.
///
/// Ports carry no type declaration, so this struct is an agreement between the
/// two ends of the link and the tap that reads it — never something the engine
/// checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequencedTick {
    pub sequence_number: u64,
    pub emitted_at_monotonic_ns: i64,
}
