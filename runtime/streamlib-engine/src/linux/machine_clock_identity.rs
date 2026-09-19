// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How Linux names the clock every stamp on this machine was taken on.

use crate::core::runtime::mesh::MachineClockIdentity;

/// This machine's clock, or [`MachineClockIdentity::UNIDENTIFIED`] when
/// `/proc` does not answer.
///
/// The boot id alone, and never the pid namespace the host identity pairs it
/// with: a container and its host share this kernel's monotonic epoch, so they
/// must read as one clock.
pub fn read_this_machines_clock_identity() -> MachineClockIdentity {
    let Some(kernel_boot_id) = crate::linux::host_identity::read_the_kernel_boot_id() else {
        tracing::debug!(
            "this machine reports no boot id, so nothing it sends across the mesh can say which \
             clock stamped it"
        );
        return MachineClockIdentity::UNIDENTIFIED;
    };
    let identity =
        MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(&kernel_boot_id);
    if identity.is_unidentified() {
        tracing::debug!(
            "this machine's boot id is not a UUID, so nothing it sends across the mesh can say \
             which clock stamped it"
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
            "a Linux machine reports a boot id, so it must name its clock: {identity:?}"
        );
        assert_eq!(identity, read_this_machines_clock_identity());
    }

    /// The clock is the boot alone: a container and its host differ in pid
    /// namespace and share a kernel, so the two identities must not be built
    /// from the same fields.
    #[test]
    fn the_clock_identity_is_the_boot_id_and_carries_no_pid_namespace() {
        let boot_id = crate::linux::host_identity::read_the_kernel_boot_id()
            .expect("a Linux machine reports a boot id");
        assert_eq!(
            read_this_machines_clock_identity(),
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(&boot_id),
            "the clock identity must be exactly what the boot id spells"
        );
    }
}
