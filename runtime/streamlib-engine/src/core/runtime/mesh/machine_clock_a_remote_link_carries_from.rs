// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which machine's monotonic clock the bags arriving on one remote link were
//! stamped on, as the mesh currently knows it.
//!
//! A stamp crosses the mesh unchanged and is never compared against a stamp
//! from another clock, so a destination reading two links has to be able to ask
//! which clock each one is on. The answer is a property of the link rather than
//! of each bag (owner, 2026-09-14): one cell per source address, written by the
//! ingress carrying it and read by `graph` and by every destination of it.
//!
//! Nothing is known until the first bag lands. The address's runtime being on
//! the mesh says nothing about the clock — a runtime announces its host, which
//! pairs a boot with a pid namespace precisely to tell a container apart from
//! the host it shares a monotonic epoch with, and that is the opposite question.

use parking_lot::Mutex;

use crate::core::runtime::mesh::machine_clock_identity::MachineClockIdentity;

/// What noting an arriving bag's clock did to what the link is carrying from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhatNotingABagsClockDid {
    /// The clock is the one the link was already carrying from.
    ItNamedTheSameMachineAgain,
    /// The first bag to cross this link named its machine.
    ItNamedTheMachineForTheFirstTime,
    /// The link is carrying from a different machine than it was, which is a
    /// peer that came back on another boot — or another machine holding the
    /// same runtime name. Its stamps are a fresh clock, so what the link had
    /// counted and reported about the old one no longer describes it.
    ItNamedAnotherMachineThanBefore {
        /// The machine the link was carrying from until this bag.
        until_this_bag: MachineClockIdentity,
    },
}

/// The clock one remote link's stamps are taken on, shared between the ingress
/// that learns it and everything that reads it.
///
/// Held by the ingress table rather than by an ingress, so it outlives the
/// source runtime leaving and coming back: the link it belongs to survives
/// that, and the cell a destination was wired with has to survive it too.
#[derive(Debug, Default)]
pub struct MachineClockARemoteLinkCarriesFrom {
    /// `None` while nothing has crossed. Not [`MachineClockIdentity::UNIDENTIFIED`]:
    /// that is a machine that named no clock of its own, which is a different
    /// answer from not having been told yet.
    carrying_from: Mutex<Option<MachineClockIdentity>>,
}

impl MachineClockARemoteLinkCarriesFrom {
    /// The machine this link's stamps are currently taken on, or `None` while
    /// nothing has crossed it.
    pub fn what_it_is_now(&self) -> Option<MachineClockIdentity> {
        *self.carrying_from.lock()
    }

    /// Take the clock one arriving bag was stamped on, and say what that did.
    pub fn note_the_machine_a_bag_was_stamped_on(
        &self,
        stamped_on: MachineClockIdentity,
    ) -> WhatNotingABagsClockDid {
        let mut carrying_from = self.carrying_from.lock();
        match carrying_from.replace(stamped_on) {
            None => WhatNotingABagsClockDid::ItNamedTheMachineForTheFirstTime,
            Some(until_this_bag) if until_this_bag == stamped_on => {
                WhatNotingABagsClockDid::ItNamedTheSameMachineAgain
            }
            Some(until_this_bag) => {
                WhatNotingABagsClockDid::ItNamedAnotherMachineThanBefore { until_this_bag }
            }
        }
    }

    /// Forget the machine, because this runtime has stopped reading the
    /// address: nothing is arriving, so no clock is being carried from.
    ///
    /// Not left at the last one it saw. A source that comes back may come back
    /// on another machine, and a link that went on rendering the old clock
    /// while carrying nothing would say something no arriving bag supports.
    pub fn forget_the_machine_because_nothing_is_arriving(&self) {
        *self.carrying_from.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_machine(boot_session_uuid: &str) -> MachineClockIdentity {
        MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(boot_session_uuid)
    }

    const ONE_MACHINE: &str = "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93";
    const ANOTHER_MACHINE: &str = "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c";

    /// Nothing is known until a bag says so: a link that has carried nothing
    /// must not answer with a clock somebody could then compare against.
    #[test]
    fn a_link_nothing_has_crossed_names_no_machine() {
        assert_eq!(
            MachineClockARemoteLinkCarriesFrom::default().what_it_is_now(),
            None
        );
    }

    /// The first bag names the machine, and every bag after it from the same
    /// machine is not a change — a change restarts the link's counting, so
    /// reading one where there is none would zero the loss count every bag.
    #[test]
    fn the_first_bag_names_the_machine_and_the_rest_say_nothing_new() {
        let carries_from = MachineClockARemoteLinkCarriesFrom::default();

        assert_eq!(
            carries_from.note_the_machine_a_bag_was_stamped_on(a_machine(ONE_MACHINE)),
            WhatNotingABagsClockDid::ItNamedTheMachineForTheFirstTime
        );
        assert_eq!(carries_from.what_it_is_now(), Some(a_machine(ONE_MACHINE)));

        for _ in 0..3 {
            assert_eq!(
                carries_from.note_the_machine_a_bag_was_stamped_on(a_machine(ONE_MACHINE)),
                WhatNotingABagsClockDid::ItNamedTheSameMachineAgain
            );
        }
        assert_eq!(carries_from.what_it_is_now(), Some(a_machine(ONE_MACHINE)));
    }

    /// A peer back on another boot is another clock, and the answer names both
    /// machines so whoever acts on it can say what changed.
    #[test]
    fn a_bag_from_another_machine_is_a_change_naming_the_one_before_it() {
        let carries_from = MachineClockARemoteLinkCarriesFrom::default();
        carries_from.note_the_machine_a_bag_was_stamped_on(a_machine(ONE_MACHINE));

        assert_eq!(
            carries_from.note_the_machine_a_bag_was_stamped_on(a_machine(ANOTHER_MACHINE)),
            WhatNotingABagsClockDid::ItNamedAnotherMachineThanBefore {
                until_this_bag: a_machine(ONE_MACHINE)
            }
        );
        assert_eq!(
            carries_from.what_it_is_now(),
            Some(a_machine(ANOTHER_MACHINE)),
            "the new machine is what the link carries from from now on"
        );
    }

    /// A machine that names no clock of its own is still a machine the link is
    /// carrying from, told apart from not having been told yet.
    #[test]
    fn a_machine_that_names_no_clock_is_not_the_same_as_not_knowing() {
        let carries_from = MachineClockARemoteLinkCarriesFrom::default();

        assert_eq!(
            carries_from.note_the_machine_a_bag_was_stamped_on(MachineClockIdentity::UNIDENTIFIED),
            WhatNotingABagsClockDid::ItNamedTheMachineForTheFirstTime
        );
        assert_eq!(
            carries_from.what_it_is_now(),
            Some(MachineClockIdentity::UNIDENTIFIED)
        );
    }

    /// The source leaving empties the cell: a link carrying nothing must not
    /// go on naming the machine it used to carry from, which a source that
    /// comes back elsewhere would make a lie.
    #[test]
    fn a_link_that_stops_carrying_names_no_machine_again() {
        let carries_from = MachineClockARemoteLinkCarriesFrom::default();
        carries_from.note_the_machine_a_bag_was_stamped_on(a_machine(ONE_MACHINE));

        carries_from.forget_the_machine_because_nothing_is_arriving();

        assert_eq!(carries_from.what_it_is_now(), None);
        assert_eq!(
            carries_from.note_the_machine_a_bag_was_stamped_on(a_machine(ONE_MACHINE)),
            WhatNotingABagsClockDid::ItNamedTheMachineForTheFirstTime,
            "the same machine returning after a silence is a first naming, not a change"
        );
    }
}
