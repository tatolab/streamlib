// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How Apple names the clock every stamp on this machine was taken on.

use crate::core::runtime::mesh::MachineClockIdentity;

/// The sysctl that answers with this boot session's UUID, NUL-terminated for
/// `sysctlbyname`, which takes a C string.
const BOOT_SESSION_UUID_SYSCTL: &[u8] = b"kern.bootsessionuuid\0";

/// Room for a UUID's canonical text and its terminator, with slack: the sysctl
/// states the length it wrote, and a buffer too small is an `ENOMEM` rather
/// than a truncated answer.
const HOW_MANY_BYTES_A_BOOT_SESSION_UUID_ANSWER_TAKES: usize = 64;

/// This machine's clock, or [`MachineClockIdentity::UNIDENTIFIED`] when the
/// kernel does not answer.
///
/// The boot session alone, and never the process or the sandbox: two runtimes
/// on one Mac share its monotonic epoch, so they must read as one clock.
pub fn read_this_machines_clock_identity() -> MachineClockIdentity {
    let mut answer = [0u8; HOW_MANY_BYTES_A_BOOT_SESSION_UUID_ANSWER_TAKES];
    let mut answer_length = answer.len();
    // SAFETY: the name is a NUL-terminated C string, the buffer and the length
    // cell are ours and live for the call, and no new value is set.
    let answered = unsafe {
        libc::sysctlbyname(
            BOOT_SESSION_UUID_SYSCTL.as_ptr().cast(),
            answer.as_mut_ptr().cast(),
            &mut answer_length,
            std::ptr::null_mut(),
            0,
        )
    };
    if answered != 0 {
        tracing::debug!(
            "this machine's kernel did not answer kern.bootsessionuuid, so nothing it sends \
             across the mesh can say which clock stamped it: {}",
            std::io::Error::last_os_error()
        );
        return MachineClockIdentity::UNIDENTIFIED;
    }

    // The kernel writes a NUL-terminated string and counts the terminator in
    // the length it reports, so the text is what precedes the first NUL.
    let written = &answer[..answer_length.min(answer.len())];
    let text = written.split(|byte| *byte == 0).next().unwrap_or(&[]);
    let Ok(boot_session_uuid) = std::str::from_utf8(text) else {
        tracing::debug!(
            "this machine's kern.bootsessionuuid is not text, so nothing it sends across the \
             mesh can say which clock stamped it"
        );
        return MachineClockIdentity::UNIDENTIFIED;
    };
    let identity =
        MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(boot_session_uuid);
    if identity.is_unidentified() {
        tracing::debug!(
            "this machine's kern.bootsessionuuid is not a UUID, so nothing it sends across the \
             mesh can say which clock stamped it"
        );
    }
    identity
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This kernel names its clock, and names the same one twice — a stamp's
    /// clock changing mid-run would read downstream as a peer that re-booted.
    #[test]
    fn this_kernel_names_one_clock_and_names_it_the_same_way_every_time() {
        let identity = read_this_machines_clock_identity();
        assert!(
            !identity.is_unidentified(),
            "an Apple machine answers kern.bootsessionuuid, so it must name its clock: \
             {identity:?}"
        );
        assert_eq!(identity, read_this_machines_clock_identity());
    }
}
