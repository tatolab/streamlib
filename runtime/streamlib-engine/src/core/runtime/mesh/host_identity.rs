// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What tells one host apart from another on the mesh.
//!
//! A liveliness token carries no payload, so the token key has to carry what a
//! runtime that is already dead must still answer about itself. Its host is
//! half of that: only a runtime on this very machine, sharing this process
//! table, has a pid another runtime here can go and check.

/// A host, as far as another runtime on the mesh can tell.
///
/// The derived equality is the peer table's: it keys on the whole announced
/// identity, and two `Unidentified` hosts there are the same key. Stated
/// residual: two runtimes on unidentified hosts sharing a name *and* a pid
/// collapse into one peer row. The same-host question a duplicate-name check
/// asks is a different one — an unidentified host is never this host — and
/// belongs with the check that asks it.
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
    /// A host that can be recognised again on a kernel with no pid namespaces:
    /// this kernel boot session, whose one process table every process on the
    /// boot shares. What Apple reports.
    ThisKernelBootSession {
        /// The kernel's boot session UUID, which changes on every boot.
        kernel_boot_session_uuid: String,
    },
    /// A platform that reports nothing another runtime could match against.
    /// Two `Unidentified` hosts are never the same host.
    Unidentified,
}

/// What an [`HostIdentity::Unidentified`] host renders as on a key.
const UNIDENTIFIED_HOST_CHUNK: &str = "unidentified";

/// The prefix a [`HostIdentity::ThisKernelBootAndPidNamespace`] chunk carries,
/// so a reader can tell the shapes apart without guessing at the field count.
const KERNEL_BOOT_AND_PID_NAMESPACE_HOST_CHUNK_PREFIX: &str = "kernel";

/// The prefix a [`HostIdentity::ThisKernelBootSession`] chunk carries.
const KERNEL_BOOT_SESSION_HOST_CHUNK_PREFIX: &str = "bootsession";

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
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            crate::apple::host_identity::read_this_hosts_identity()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
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
                "{KERNEL_BOOT_AND_PID_NAMESPACE_HOST_CHUNK_PREFIX}{HOST_CHUNK_FIELD_SEPARATOR}\
                 {kernel_boot_id}{HOST_CHUNK_FIELD_SEPARATOR}{pid_namespace_inode}"
            ),
            Self::ThisKernelBootSession {
                kernel_boot_session_uuid,
            } => format!(
                "{KERNEL_BOOT_SESSION_HOST_CHUNK_PREFIX}{HOST_CHUNK_FIELD_SEPARATOR}\
                 {kernel_boot_session_uuid}"
            ),
            Self::Unidentified => UNIDENTIFIED_HOST_CHUNK.to_string(),
        }
    }

    /// Whether a runtime here is the same host as `announced_host`, in the
    /// sense the same-host exception needs: a pid announced by that host is one
    /// this host's process table can be asked about.
    ///
    /// [`HostIdentity::Unidentified`] is never this host, on either side. A
    /// platform that recognises no host recognises none of its own runtimes
    /// either, so the exception never fires there without a `#[cfg]` spelling
    /// it that way.
    pub fn is_the_same_host_a_pid_can_be_checked_on(&self, announced_host: &Self) -> bool {
        !matches!(self, Self::Unidentified) && self == announced_host
    }

    /// The identity a key chunk carries, or `None` when the chunk is not one
    /// this engine wrote.
    pub fn from_one_key_chunk(chunk: &str) -> Option<Self> {
        if chunk == UNIDENTIFIED_HOST_CHUNK {
            return Some(Self::Unidentified);
        }
        let mut fields = chunk.split(HOST_CHUNK_FIELD_SEPARATOR);
        let identity = match fields.next()? {
            KERNEL_BOOT_AND_PID_NAMESPACE_HOST_CHUNK_PREFIX => {
                let kernel_boot_id = fields.next()?.to_string();
                let pid_namespace_inode = fields.next()?.parse().ok()?;
                if kernel_boot_id.is_empty() {
                    return None;
                }
                Self::ThisKernelBootAndPidNamespace {
                    kernel_boot_id,
                    pid_namespace_inode,
                }
            }
            KERNEL_BOOT_SESSION_HOST_CHUNK_PREFIX => {
                let kernel_boot_session_uuid = fields.next()?.to_string();
                if kernel_boot_session_uuid.is_empty() {
                    return None;
                }
                Self::ThisKernelBootSession {
                    kernel_boot_session_uuid,
                }
            }
            _ => return None,
        };
        if fields.next().is_some() {
            return None;
        }
        Some(identity)
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

    fn a_boot_session(kernel_boot_session_uuid: &str) -> HostIdentity {
        HostIdentity::ThisKernelBootSession {
            kernel_boot_session_uuid: kernel_boot_session_uuid.to_string(),
        }
    }

    /// Every shape survives the key and comes back as what it was.
    #[test]
    fn every_identity_round_trips_through_its_key_chunk() {
        for identity in [
            HostIdentity::of_this_host(),
            identified("2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93", 4_026_531_836),
            a_boot_session("2F1C8A30-6B4E-4D5A-9A11-2C7F0D5E8B93"),
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
            a_boot_session("2F1C8A30-6B4E-4D5A-9A11-2C7F0D5E8B93"),
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
        assert_ne!(a_boot_session("boot"), a_boot_session("other-boot"));
        assert_ne!(a_boot_session("boot"), HostIdentity::Unidentified);
    }

    /// A platform that reports a host identifies itself, and the same way
    /// twice — the property a restart racing its predecessor's exit depends on.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn this_host_is_identified_and_the_same_host_every_time() {
        let here = HostIdentity::of_this_host();
        assert_ne!(here, HostIdentity::Unidentified);
        assert_eq!(here, HostIdentity::of_this_host());
        assert!(here.is_the_same_host_a_pid_can_be_checked_on(&HostIdentity::of_this_host()));
    }

    /// A Linux host and an Apple host never read as one, whatever their boot
    /// ids spell: the two shapes are different keys.
    #[test]
    fn a_linux_host_is_never_an_apple_host() {
        let linux = identified("2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93", 4_026_531_836);
        let apple = a_boot_session("2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93");
        assert_ne!(linux, apple);
        assert!(!linux.is_the_same_host_a_pid_can_be_checked_on(&apple));
        assert!(!apple.is_the_same_host_a_pid_can_be_checked_on(&linux));
    }

    /// The same-host question the duplicate-name exception asks, which is not
    /// the peer table's equality: an unidentified host is never this host, so a
    /// platform that recognises nothing never takes a name over.
    #[test]
    fn only_an_identified_host_is_ever_the_same_host_a_pid_can_be_checked_on() {
        let here = identified("boot", 1);

        assert!(here.is_the_same_host_a_pid_can_be_checked_on(&identified("boot", 1)));
        assert!(!here.is_the_same_host_a_pid_can_be_checked_on(&identified("boot", 2)));
        assert!(!here.is_the_same_host_a_pid_can_be_checked_on(&identified("other-boot", 1)));
        assert!(!here.is_the_same_host_a_pid_can_be_checked_on(&HostIdentity::Unidentified));

        let this_boot_session = a_boot_session("boot");
        assert!(
            this_boot_session.is_the_same_host_a_pid_can_be_checked_on(&a_boot_session("boot"))
        );
        assert!(
            !this_boot_session
                .is_the_same_host_a_pid_can_be_checked_on(&a_boot_session("other-boot"))
        );
        assert!(
            !this_boot_session
                .is_the_same_host_a_pid_can_be_checked_on(&HostIdentity::Unidentified)
        );

        assert!(
            !HostIdentity::Unidentified
                .is_the_same_host_a_pid_can_be_checked_on(&HostIdentity::Unidentified),
            "a host that recognises nothing must not recognise itself"
        );
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
            "bootsession",
            "bootsession.",
            "bootsession.boot.7",
        ] {
            assert_eq!(
                HostIdentity::from_one_key_chunk(foreign),
                None,
                "{foreign:?} must read as no identity"
            );
        }
    }
}
