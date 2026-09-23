// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How Apple tells this host apart from every other host on the mesh.

use crate::core::runtime::mesh::HostIdentity;

/// The sysctl that answers with this boot session's UUID, NUL-terminated for
/// `sysctlbyname`, which takes a C string.
const BOOT_SESSION_UUID_SYSCTL: &[u8] = b"kern.bootsessionuuid\0";

/// Room for a UUID's canonical text and its terminator, with slack: the sysctl
/// states the length it wrote, and a buffer too small is an `ENOMEM` rather
/// than a truncated answer.
const HOW_MANY_BYTES_A_BOOT_SESSION_UUID_ANSWER_TAKES: usize = 64;

/// This host, or [`HostIdentity::Unidentified`] when the kernel does not
/// answer.
///
/// The boot session alone: Darwin has no pid namespaces, so every process on
/// this boot shares one process table and the boot session is the whole of
/// what a pid needs to be checkable here.
pub fn read_this_hosts_identity() -> HostIdentity {
    read_the_kernel_boot_session_uuid().map_or(
        HostIdentity::Unidentified,
        |kernel_boot_session_uuid| HostIdentity::ThisKernelBootSession {
            kernel_boot_session_uuid,
        },
    )
}

/// The kernel's boot session UUID as this machine reports it, or `None` when
/// the kernel does not answer. The one read site the machine clock identity
/// shares; a failure is logged here, once.
pub fn read_the_kernel_boot_session_uuid() -> Option<String> {
    let mut answer = [0u8; HOW_MANY_BYTES_A_BOOT_SESSION_UUID_ANSWER_TAKES];
    let mut answer_length = answer.len();
    // SAFETY: the name is a NUL-terminated C string, the buffer and the length
    // cell are ours and live for the call, and no new value is set. The length
    // cell is initialized to the buffer's own capacity, which is what bounds
    // what the kernel may write into it.
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
            "this machine's kernel did not answer kern.bootsessionuuid, so it names neither its \
             host nor its clock on the mesh: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    the_boot_session_uuid_the_kernel_wrote(&answer[..answer_length.min(answer.len())])
}

/// The text of a sysctl answer.
///
/// The kernel writes a NUL-terminated string and counts the terminator in the
/// length it reports, so the text is what precedes the first NUL.
fn the_boot_session_uuid_the_kernel_wrote(written: &[u8]) -> Option<String> {
    let text = written.split(|byte| *byte == 0).next().unwrap_or(&[]);
    let Ok(boot_session_uuid) = std::str::from_utf8(text) else {
        tracing::debug!(
            "this machine's kern.bootsessionuuid is not text, so it names neither its host nor \
             its clock on the mesh"
        );
        return None;
    };
    let boot_session_uuid = boot_session_uuid.trim();
    (!boot_session_uuid.is_empty()).then(|| boot_session_uuid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::mesh::MachineClockIdentity;

    /// This kernel identifies itself, and does so the same way twice — the
    /// property a restart racing its predecessor's exit depends on.
    #[test]
    fn this_kernel_identifies_itself_the_same_way_every_time() {
        let identity = read_this_hosts_identity();
        assert!(
            matches!(identity, HostIdentity::ThisKernelBootSession { .. }),
            "an Apple host answers kern.bootsessionuuid, so it must identify itself: \
             {identity:?}"
        );
        assert_eq!(identity, read_this_hosts_identity());
    }

    /// The host and the clock are named by one UUID, read at one site: a host
    /// whose clock identity said another boot would be two machines at once.
    #[test]
    fn the_host_identity_is_the_boot_session_the_clock_identity_names() {
        let HostIdentity::ThisKernelBootSession {
            kernel_boot_session_uuid,
        } = read_this_hosts_identity()
        else {
            panic!("an Apple host must identify itself");
        };
        assert_eq!(
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                &kernel_boot_session_uuid
            ),
            crate::apple::machine_clock_identity::read_this_machines_clock_identity(),
        );
    }

    /// What the kernel wrote is read up to its terminator, and an answer with
    /// no text in it is no boot session rather than an empty one.
    #[test]
    fn the_answer_is_the_text_before_the_terminator_and_empty_text_is_none() {
        assert_eq!(
            the_boot_session_uuid_the_kernel_wrote(b"2F1C8A30-6B4E-4D5A-9A11-2C7F0D5E8B93\0\0\0")
                .as_deref(),
            Some("2F1C8A30-6B4E-4D5A-9A11-2C7F0D5E8B93")
        );
        assert_eq!(the_boot_session_uuid_the_kernel_wrote(b"\0"), None);
        assert_eq!(the_boot_session_uuid_the_kernel_wrote(b""), None);
        assert_eq!(the_boot_session_uuid_the_kernel_wrote(b"\xff\xfe\0"), None);
    }
}
