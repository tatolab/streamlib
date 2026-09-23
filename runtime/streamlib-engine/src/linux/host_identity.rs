// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How Linux tells this host apart from every other host on the mesh.

use std::path::Path;

use crate::core::runtime::mesh::HostIdentity;

/// The kernel's own boot id, regenerated on every boot.
const KERNEL_BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

/// This process's pid namespace, as a symlink whose inode names the namespace.
/// Two containers on one kernel have different inodes here, so a pid read off
/// one of their tokens is never checked against this host's process table.
const PID_NAMESPACE_PATH: &str = "/proc/self/ns/pid";

/// This host, or [`HostIdentity::Unidentified`] when `/proc` does not answer.
///
/// Unidentified rather than a fallback value: a wrong answer here makes a
/// runtime on another machine look like a local one, and no answer at all
/// only costs the same-host exception.
pub fn read_this_hosts_identity() -> HostIdentity {
    read_the_identity_of(
        Path::new(KERNEL_BOOT_ID_PATH),
        Path::new(PID_NAMESPACE_PATH),
    )
}

/// The kernel's boot id as this machine reports it, or `None` when `/proc`
/// does not answer.
///
/// Shared with the machine clock identity, which is this same boot id and
/// nothing else: one read site and one path constant, because a second reader
/// of one file is a second answer waiting to disagree.
pub fn read_the_kernel_boot_id() -> Option<String> {
    read_the_kernel_boot_id_at(Path::new(KERNEL_BOOT_ID_PATH))
}

/// The read with its path named, so the failure arm is testable without a
/// second kernel.
fn read_the_kernel_boot_id_at(kernel_boot_id_path: &Path) -> Option<String> {
    let kernel_boot_id = std::fs::read_to_string(kernel_boot_id_path).ok()?;
    let kernel_boot_id = kernel_boot_id.trim().to_string();
    (!kernel_boot_id.is_empty()).then_some(kernel_boot_id)
}

/// The reader with both paths named, so the failure arms are testable without
/// a second kernel.
fn read_the_identity_of(kernel_boot_id_path: &Path, pid_namespace_path: &Path) -> HostIdentity {
    let Some(kernel_boot_id) = read_the_kernel_boot_id_at(kernel_boot_id_path) else {
        tracing::debug!(
            "this host reports no boot id at {}; mesh peers here are all remote to each other",
            kernel_boot_id_path.display()
        );
        return HostIdentity::Unidentified;
    };

    // The inode, not the link's text: `/proc/self/ns/pid` reads as
    // `pid:[4026531836]`, and the number inside it is the inode `stat` reports
    // directly.
    let Ok(pid_namespace) = std::fs::metadata(pid_namespace_path) else {
        tracing::debug!(
            "this host reports no pid namespace at {}; mesh peers here are all remote to each \
             other",
            pid_namespace_path.display()
        );
        return HostIdentity::Unidentified;
    };

    HostIdentity::ThisKernelBootAndPidNamespace {
        kernel_boot_id,
        pid_namespace_inode: std::os::unix::fs::MetadataExt::ino(&pid_namespace),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This kernel identifies itself, and does so the same way twice — the
    /// property a restart racing its predecessor's exit depends on.
    #[test]
    fn this_kernel_identifies_itself_the_same_way_every_time() {
        let identity = read_this_hosts_identity();
        assert!(
            matches!(identity, HostIdentity::ThisKernelBootAndPidNamespace { .. }),
            "a Linux host must identify itself: {identity:?}"
        );
        assert_eq!(identity, read_this_hosts_identity());
    }

    /// A host whose `/proc` does not answer is unidentified rather than given
    /// a stand-in another machine could match.
    #[test]
    fn a_host_whose_proc_does_not_answer_is_unidentified() {
        let absent = Path::new("/nonexistent/streamlib/boot_id");
        assert_eq!(
            read_the_identity_of(absent, Path::new(PID_NAMESPACE_PATH)),
            HostIdentity::Unidentified
        );
        assert_eq!(
            read_the_identity_of(Path::new(KERNEL_BOOT_ID_PATH), absent),
            HostIdentity::Unidentified
        );
    }
}
