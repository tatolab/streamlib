// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How Apple names the clock every stamp on this machine was taken on.

use crate::core::runtime::mesh::MachineClockIdentity;

/// This machine's clock, or [`MachineClockIdentity::UNIDENTIFIED`] when the
/// kernel does not answer.
///
/// The boot session alone, and never the process or the sandbox: two runtimes
/// on one Mac share its monotonic epoch, so they must read as one clock.
pub fn read_this_machines_clock_identity() -> MachineClockIdentity {
    let Some(boot_session_uuid) = crate::apple::host_identity::read_the_kernel_boot_session_uuid()
    else {
        return MachineClockIdentity::UNIDENTIFIED;
    };
    let identity =
        MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(&boot_session_uuid);
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
