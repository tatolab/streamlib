// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unique-name minting for machine-global namespaces.
//!
//! iceoryx2 service names live in `/dev/shm`, which is machine-global and
//! outlives the process that created an entry. A name that collides with a
//! concurrent process — or with a stale entry an earlier run left behind after
//! the pid was recycled — surfaces as `DoesNotSupportRequestedMinBufferSize`
//! against the wrong service, not as a clean failure.
//!
//! Nothing minted here is a timestamp: the monotonic read only has to differ
//! between two runs on one boot, the pid only has to differ between concurrent
//! processes, and the sequence makes two mints in the same nanosecond distinct —
//! which a clock read alone never guarantees.
//!
//! The suffix carries no separator a path or a file name would reject, so the
//! caller composes it into whatever naming convention its namespace uses.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::media_clock::MediaClock;

static MINT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A `<pid>-<monotonic_ns>-<sequence>` suffix no concurrent process, and no
/// earlier run that recycled this pid, can collide with.
pub fn mint_machine_global_unique_name_suffix() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        MediaClock::now().as_nanos(),
        MINT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn successive_mints_never_repeat() {
        let minted: HashSet<String> = (0..10_000)
            .map(|_| mint_machine_global_unique_name_suffix())
            .collect();
        assert_eq!(
            minted.len(),
            10_000,
            "two mints collided — a tight loop reads the same monotonic nanosecond, so the \
             sequence is what makes them distinct"
        );
    }

    #[test]
    fn carries_no_separator_a_path_or_file_name_would_reject() {
        let minted = mint_machine_global_unique_name_suffix();
        assert!(
            !minted.contains(['/', '\\', '.', ' ']),
            "callers compose this into iceoryx2 service paths and into file names: {minted}"
        );
        assert!(
            minted.starts_with(&format!("{}-", std::process::id())),
            "a stale entry names the process that left it: {minted}"
        );
    }
}
