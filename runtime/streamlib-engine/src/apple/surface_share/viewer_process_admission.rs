// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A viewer's process admitted to the surface-sharing Mach service: a running
//! process of the user this runtime runs as, named by whoever opened the
//! viewer over the local API.

use super::{MachSurfaceShareServiceRendezvous, SurfaceShareHelperProcessAdmission};
use crate::core::error::{Error, Result};

/// Admit process `viewer_process_id` to the service, refused unless it is a
/// running process of the user this runtime runs as.
pub fn admit_a_viewer_process(
    rendezvous: &MachSurfaceShareServiceRendezvous,
    viewer_process_id: u32,
) -> Result<SurfaceShareHelperProcessAdmission> {
    let refused = |why: &str| {
        Err(Error::Link(format!(
            "viewer process {viewer_process_id} is not admitted to the surface-sharing service: \
             {why}"
        )))
    };
    let Ok(pid) = libc::pid_t::try_from(viewer_process_id) else {
        return refused("no process has that id");
    };
    if pid <= 0 {
        return refused("no process has that id");
    }
    let Some(owning_user) = owning_user_of_pid(pid) else {
        return refused("no running process has that id");
    };
    // SAFETY: `geteuid` reads this process's effective user and cannot fail.
    if owning_user != unsafe { libc::geteuid() } {
        return refused("it runs as another user than this runtime");
    }
    Ok(rendezvous.admit_helper_process(viewer_process_id))
}

fn owning_user_of_pid(pid: libc::pid_t) -> Option<libc::uid_t> {
    // SAFETY: `proc_bsdinfo` is plain data for which all-zero is valid.
    let mut process_info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let process_info_size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `process_info` is a writable `proc_bsdinfo`, the size passed is
    // its own, and `PROC_PIDTBSDINFO` fills exactly that struct.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut process_info as *mut libc::proc_bsdinfo).cast(),
            process_info_size,
        )
    };
    (written == process_info_size).then_some(process_info.pbi_uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};

    fn a_rendezvous() -> MachSurfaceShareServiceRendezvous {
        MachSurfaceShareService::new(
            IOSurfaceShareState::new(),
            "com.tatolab.streamlib.viewer-admission-test".to_string(),
        )
        .rendezvous()
    }

    #[test]
    fn this_users_running_process_is_admitted() {
        assert!(admit_a_viewer_process(&a_rendezvous(), std::process::id()).is_ok());
    }

    #[test]
    fn an_id_no_process_has_is_refused() {
        for viewer_process_id in [0, u32::MAX, i32::MAX as u32] {
            assert!(
                admit_a_viewer_process(&a_rendezvous(), viewer_process_id).is_err(),
                "{viewer_process_id}"
            );
        }
    }

    #[test]
    fn another_users_process_is_refused() {
        // SAFETY: `geteuid` reads this process's effective user and cannot fail.
        if unsafe { libc::geteuid() } == 1 {
            return;
        }
        assert!(
            admit_a_viewer_process(&a_rendezvous(), 1).is_err(),
            "launchd runs as root"
        );
    }
}
