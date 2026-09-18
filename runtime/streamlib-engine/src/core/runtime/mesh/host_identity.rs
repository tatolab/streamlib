// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What tells one host apart from another on the mesh.
//!
//! A liveliness token carries no payload, so the token key has to carry what a
//! runtime that is already dead must still answer about itself. Its host is
//! half of that: only a runtime on this very machine, in this very pid
//! namespace, has a pid another runtime here can go and check.

/// A host, as far as another runtime on the mesh can tell.
///
/// The derived equality is the peer table's: it keys on the whole announced
/// identity, and two `Unidentified` hosts there are the same key. Stated
/// residual: two macOS runtimes sharing a name *and* a pid collapse into one
/// peer row. The same-host question a duplicate-name check asks is a different
/// one — an unidentified host is never this host — and belongs with the check
/// that asks it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostIdentity {
    /// A host that can be recognised again: this kernel boot and this pid
    /// namespace. A container on the same kernel has another pid namespace, so
    /// it is never mistaken for this host.
    ThisKernelBootAndPidNamespace {
        /// The kernel's boot id, which changes on every reboot.
        kernel_boot_id: String,
        /// The inode of this process's pid namespace.
        pid_namespace_inode: u64,
    },
    /// A platform that reports nothing another runtime could match against.
    /// Two `Unidentified` hosts are never the same host.
    Unidentified,
}

/// What an [`HostIdentity::Unidentified`] host renders as on a key.
const UNIDENTIFIED_HOST_CHUNK: &str = "unidentified";

/// The prefix an identified host's chunk carries, so a reader can tell the two
/// shapes apart without guessing at the field count.
const IDENTIFIED_HOST_CHUNK_PREFIX: &str = "kernel";

/// What separates the fields inside one host chunk. A `.` because the boot id
/// carries `-` and the chunk has to stay parseable; both are legal in a key
/// chunk, which forbids only `/ * $ ? #`.
const HOST_CHUNK_FIELD_SEPARATOR: char = '.';

impl HostIdentity {
    /// This host, as this platform can report it.
    pub fn of_this_host() -> Self {
        #[cfg(target_os = "linux")]
        {
            crate::linux::host_identity::read_this_hosts_identity()
        }
        // Apple reports no boot id and no pid namespace, so a runtime there
        // recognises no host — including its own. The duplicate-name exception
        // that reads this is Linux-only for exactly that reason.
        #[cfg(not(target_os = "linux"))]
        {
            Self::Unidentified
        }
    }

    /// This identity as one key chunk.
    pub fn as_one_key_chunk(&self) -> String {
        match self {
            Self::ThisKernelBootAndPidNamespace {
                kernel_boot_id,
                pid_namespace_inode,
            } => format!(
                "{IDENTIFIED_HOST_CHUNK_PREFIX}{HOST_CHUNK_FIELD_SEPARATOR}{kernel_boot_id}\
                 {HOST_CHUNK_FIELD_SEPARATOR}{pid_namespace_inode}"
            ),
            Self::Unidentified => UNIDENTIFIED_HOST_CHUNK.to_string(),
        }
    }

    /// The identity a key chunk carries, or `None` when the chunk is not one
    /// this engine wrote.
    pub fn from_one_key_chunk(chunk: &str) -> Option<Self> {
        if chunk == UNIDENTIFIED_HOST_CHUNK {
            return Some(Self::Unidentified);
        }
        let mut fields = chunk.split(HOST_CHUNK_FIELD_SEPARATOR);
        if fields.next()? != IDENTIFIED_HOST_CHUNK_PREFIX {
            return None;
        }
        let kernel_boot_id = fields.next()?.to_string();
        let pid_namespace_inode = fields.next()?.parse().ok()?;
        if fields.next().is_some() || kernel_boot_id.is_empty() {
            return None;
        }
        Some(Self::ThisKernelBootAndPidNamespace {
            kernel_boot_id,
            pid_namespace_inode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::mesh_address_chunk::first_reason_this_is_not_one_mesh_address_chunk;

    fn identified(kernel_boot_id: &str, pid_namespace_inode: u64) -> HostIdentity {
        HostIdentity::ThisKernelBootAndPidNamespace {
            kernel_boot_id: kernel_boot_id.to_string(),
            pid_namespace_inode,
        }
    }

    /// Both shapes survive the key and come back as what they were.
    #[test]
    fn every_identity_round_trips_through_its_key_chunk() {
        for identity in [
            identified("2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93", 4_026_531_836),
            HostIdentity::Unidentified,
        ] {
            let chunk = identity.as_one_key_chunk();
            assert_eq!(
                HostIdentity::from_one_key_chunk(&chunk),
                Some(identity.clone()),
                "{chunk} must read back as the identity that wrote it"
            );
        }
    }

    /// Whatever a host reports, the chunk it renders is one legal key chunk —
    /// otherwise the token could not be declared at all.
    #[test]
    fn every_identitys_chunk_is_one_legal_key_chunk() {
        for identity in [
            HostIdentity::of_this_host(),
            identified("2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93", 4_026_531_836),
            HostIdentity::Unidentified,
        ] {
            let chunk = identity.as_one_key_chunk();
            assert_eq!(
                first_reason_this_is_not_one_mesh_address_chunk(&chunk),
                None,
                "{chunk} must be one legal key chunk"
            );
        }
    }

    /// A different boot or a different pid namespace is a different key, so a
    /// container on this kernel never shares a peer row with its host.
    #[test]
    fn a_different_boot_or_pid_namespace_is_a_different_identity() {
        assert_eq!(identified("boot", 1), identified("boot", 1));
        assert_ne!(identified("boot", 1), identified("boot", 2));
        assert_ne!(identified("boot", 1), identified("other-boot", 1));
        assert_ne!(identified("boot", 1), HostIdentity::Unidentified);
    }

    /// A chunk this engine did not write reads as nothing, rather than as a
    /// host that would then be compared against.
    #[test]
    fn a_chunk_this_engine_did_not_write_reads_as_no_identity() {
        for foreign in [
            "",
            "kernel",
            "kernel.boot",
            "kernel.boot.notanumber",
            "kernel..7",
            "other.boot.7",
        ] {
            assert_eq!(
                HostIdentity::from_one_key_chunk(foreign),
                None,
                "{foreign:?} must read as no identity"
            );
        }
    }
}
