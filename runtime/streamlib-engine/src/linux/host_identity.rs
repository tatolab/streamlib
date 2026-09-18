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

/// The reader with both paths named, so the failure arms are testable without
/// a second kernel.
fn read_the_identity_of(kernel_boot_id_path: &Path, pid_namespace_path: &Path) -> HostIdentity {
    let Ok(kernel_boot_id) = std::fs::read_to_string(kernel_boot_id_path) else {
        tracing::debug!(
            "this host reports no boot id at {}; mesh peers here are all remote to each other",
            kernel_boot_id_path.display()
        );
        return HostIdentity::Unidentified;
    };
    let kernel_boot_id = kernel_boot_id.trim().to_string();
    if kernel_boot_id.is_empty() {
        return HostIdentity::Unidentified;
    }

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

/// Whether the process `process_id` names has left this host's process table.
///
/// Only ever asked about a pid read off a token whose host identity equals this
/// host's, so the pid is in this process's own namespace and `kill` can see it.
/// A pid the kernel still knows — `EPERM`, another user's process, included —
/// is still there, and anything but `ESRCH` is read that way: the refusal is
/// the safe answer, and a name taken over from a live runtime is not
/// recoverable.
pub fn a_process_on_this_host_is_gone(process_id: u32) -> bool {
    let Ok(process_id) = libc::pid_t::try_from(process_id) else {
        return false;
    };
    // SAFETY: signal 0 delivers nothing; `kill` only reports reachability.
    let signalled = unsafe { libc::kill(process_id, 0) };
    a_process_is_gone_when_signalling_it_said(
        signalled,
        std::io::Error::last_os_error().raw_os_error(),
    )
}

/// What `kill`'s answer means, split out because the interesting arm is the one
/// a test cannot choose to get: whether this process may signal pid 1 depends
/// on whether it is root, so asking the kernel does not exercise `EPERM`.
fn a_process_is_gone_when_signalling_it_said(
    signalled: libc::c_int,
    errno: Option<libc::c_int>,
) -> bool {
    signalled != 0 && errno == Some(libc::ESRCH)
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

    /// The probe the same-host exception rests on: a process that has exited
    /// is gone, and one that is running — this very test — is not.
    #[test]
    fn a_reaped_process_is_gone_and_a_running_one_is_not() {
        let mut exited = std::process::Command::new("true")
            .spawn()
            .expect("a process this host can run");
        let reaped_process_id = exited.id();
        exited.wait().expect("the process is reaped");

        assert!(a_process_on_this_host_is_gone(reaped_process_id));
        assert!(!a_process_on_this_host_is_gone(std::process::id()));
    }

    /// Only `ESRCH` frees a name. `EPERM` — a process this one may not signal —
    /// is a process that is still there, and so is any other errno: the refusal
    /// is the recoverable answer, and a name taken from a live runtime is not.
    #[test]
    fn only_no_such_process_means_gone_and_every_other_answer_means_still_there() {
        assert!(a_process_is_gone_when_signalling_it_said(
            -1,
            Some(libc::ESRCH)
        ));

        assert!(!a_process_is_gone_when_signalling_it_said(0, None));
        assert!(!a_process_is_gone_when_signalling_it_said(
            -1,
            Some(libc::EPERM)
        ));
        assert!(!a_process_is_gone_when_signalling_it_said(
            -1,
            Some(libc::EINVAL)
        ));
        assert!(!a_process_is_gone_when_signalling_it_said(-1, None));
    }

    /// Pid 1 is always there, whether this process may signal it or not — the
    /// two answers the kernel gives for it are covered above.
    #[test]
    fn pid_one_is_never_read_as_gone() {
        assert!(!a_process_on_this_host_is_gone(1));
    }

    /// A number no pid could be is not read as a free name.
    #[test]
    fn a_number_that_is_no_pid_at_all_is_not_read_as_gone() {
        assert!(!a_process_on_this_host_is_gone(u32::MAX));
    }
}
