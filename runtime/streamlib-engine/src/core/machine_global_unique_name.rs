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
//! Nothing minted here is a timestamp. A v4 UUID's 122 random bits make a
//! collision between two mints vanishingly improbable — not impossible, but far
//! below the rate at which the namespace itself fails — across processes, runs
//! and reboots alike, so no clock is read and no counter is kept. The version
//! matters: `now_v7` and friends are timestamp-based, and reaching for one would
//! put a wall-clock read back into this module. The pid prefix is diagnostic —
//! it is what lets someone reading a stale `/dev/shm` entry or a leftover `/tmp`
//! file name the process that left it.
//!
//! The suffix carries no separator a path or a file name would reject, so the
//! caller composes it into whatever naming convention its namespace uses.

use uuid::Uuid;

/// A `<pid>-<uuid v4>` suffix that no concurrent process, and no earlier run,
/// will collide with short of exhausting 122 bits of randomness.
pub fn mint_machine_global_unique_name_suffix() -> String {
    format!("{}-{}", std::process::id(), Uuid::new_v4())
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
        assert_eq!(minted.len(), 10_000, "two mints collided");
    }

    /// The pid is a fixed prefix within one process, so distinctness can only
    /// come from the UUID — this fails if that component is ever dropped or
    /// made constant, which a whole-string equality test does not catch.
    #[test]
    fn the_uuid_component_is_what_differs_between_two_mints() {
        let first = mint_machine_global_unique_name_suffix();
        let second = mint_machine_global_unique_name_suffix();

        let pid_prefix = format!("{}-", std::process::id());
        let first_uuid = first.strip_prefix(&pid_prefix).expect(&first);
        let second_uuid = second.strip_prefix(&pid_prefix).expect(&second);

        assert_ne!(first_uuid, second_uuid);

        let parsed = Uuid::parse_str(first_uuid).expect(&first);
        assert_eq!(
            parsed.get_version_num(),
            4,
            "randomness is the whole contract: the timestamp-based versions would put a \
             wall-clock read back into this mint. The `uuid` dependency enabling only `v4` \
             is the first guard and the compiler enforces it; this is the one that survives \
             another feature being switched on for an unrelated reason"
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
