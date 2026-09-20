// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which machine's monotonic clock the bags arriving on one inbound link were
//! stamped on.
//!
//! Every stamp on the data plane is a machine's monotonic clock, whose epoch is
//! that machine's own boot, so two stamps from two machines are readings of two
//! unrelated clocks and subtracting them means nothing. A destination fanning
//! several links in therefore has to be able to ask, link by link, which clock
//! it is reading — and the answer belongs to the link rather than to each bag
//! (owner, 2026-09-14).
//!
//! What a process can answer depends on where it is. The app process holds the
//! mesh, so it reads the ingress's own cell; a helper process holds none, and
//! for a link carrying from another runtime it can only say so.

use std::sync::Arc;

use crate::core::runtime::mesh::{MachineClockARemoteLinkCarriesFrom, MachineClockIdentity};

/// Where one inbound link's binding reads the clock its stamps are taken on.
///
/// Given at wire time, because that is when the link's source is known to be on
/// this runtime or on another one. What the cell holds changes afterwards; which
/// cell it is does not.
#[derive(Clone)]
pub enum TheClockAnInboundLinksStampsAreTakenOn {
    /// A link from a processor on this runtime. Its bags were stamped here, so
    /// the answer is this machine and never changes.
    ThisMachine,
    /// A link carrying from another runtime, bound in the app process, which
    /// holds the ingress that learns the sending machine off each arriving bag.
    WhicheverMachineTheMeshIsCarryingFrom(Arc<MachineClockARemoteLinkCarriesFrom>),
    /// A link carrying from another runtime, bound in a helper process. A
    /// helper opens no mesh session, so nothing here can name the machine and
    /// the reader is sent to the app process for it.
    AMachineOnlyTheAppProcessCanName,
}

impl TheClockAnInboundLinksStampsAreTakenOn {
    /// What this process can say about the clock right now.
    pub fn what_is_known_of_it(&self) -> WhatIsKnownOfAnInboundLinksStampClock {
        match self {
            Self::ThisMachine => WhatIsKnownOfAnInboundLinksStampClock::TheMachine(
                MachineClockIdentity::of_this_machine(),
            ),
            Self::WhicheverMachineTheMeshIsCarryingFrom(carries_from) => {
                match carries_from.what_it_is_now() {
                    Some(machine) => WhatIsKnownOfAnInboundLinksStampClock::TheMachine(machine),
                    None => WhatIsKnownOfAnInboundLinksStampClock::NothingHasCrossedItYet,
                }
            }
            Self::AMachineOnlyTheAppProcessCanName => {
                WhatIsKnownOfAnInboundLinksStampClock::OnlyTheAppProcessCanSay
            }
        }
    }
}

/// The answer to "which machine's clock are this link's stamps taken on".
///
/// Four answers rather than an identity or nothing, because "not yet", "not
/// here" and "no such link" are three different things and a reader that
/// treats them alike either compares stamps it must not or refuses to compare
/// stamps it may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhatIsKnownOfAnInboundLinksStampClock {
    /// The machine whose monotonic clock stamped the bags arriving on it.
    TheMachine(MachineClockIdentity),
    /// It carries from another runtime and nothing has crossed it yet, so no
    /// bag has said which machine stamped it.
    NothingHasCrossedItYet,
    /// It carries from another runtime and is being read in a helper process,
    /// which holds no mesh session. The app process can answer.
    OnlyTheAppProcessCanSay,
    /// No link of that name feeds that port.
    NoSuchLinkFeedsThatPort,
}

impl WhatIsKnownOfAnInboundLinksStampClock {
    /// The machine, where one is known here, and `None` for every answer that
    /// names none — for a caller that has nothing different to do with the
    /// three ways of not knowing.
    pub fn the_machine_if_it_is_known(self) -> Option<MachineClockIdentity> {
        match self {
            Self::TheMachine(machine) => Some(machine),
            Self::NothingHasCrossedItYet
            | Self::OnlyTheAppProcessCanSay
            | Self::NoSuchLinkFeedsThatPort => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANOTHER_MACHINE: &str = "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c";

    /// A link from this runtime answers this machine, with nothing to wait for
    /// — it is what a local destination compares a remote link against.
    #[test]
    fn a_link_from_this_runtime_answers_this_machine() {
        assert_eq!(
            TheClockAnInboundLinksStampsAreTakenOn::ThisMachine.what_is_known_of_it(),
            WhatIsKnownOfAnInboundLinksStampClock::TheMachine(
                MachineClockIdentity::of_this_machine()
            )
        );
    }

    /// A remote link answers what the ingress has learnt, and says so rather
    /// than guessing while it has learnt nothing.
    #[test]
    fn a_remote_link_answers_nothing_until_a_bag_has_crossed_it() {
        let carries_from = Arc::new(MachineClockARemoteLinkCarriesFrom::default());
        let clock = TheClockAnInboundLinksStampsAreTakenOn::WhicheverMachineTheMeshIsCarryingFrom(
            Arc::clone(&carries_from),
        );

        assert_eq!(
            clock.what_is_known_of_it(),
            WhatIsKnownOfAnInboundLinksStampClock::NothingHasCrossedItYet
        );

        let another_machine =
            MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(ANOTHER_MACHINE);
        carries_from.note_the_machine_a_bag_was_stamped_on(another_machine);

        assert_eq!(
            clock.what_is_known_of_it(),
            WhatIsKnownOfAnInboundLinksStampClock::TheMachine(another_machine),
            "the binding reads the ingress's cell, so it follows what arrives"
        );
    }

    /// A helper holds no mesh, so it sends the reader to the app process
    /// instead of answering with this machine — which would be wrong, and
    /// wrong in the direction that lets two clocks be compared.
    #[test]
    fn a_remote_link_in_a_helper_names_no_machine_of_its_own() {
        assert_eq!(
            TheClockAnInboundLinksStampsAreTakenOn::AMachineOnlyTheAppProcessCanName
                .what_is_known_of_it(),
            WhatIsKnownOfAnInboundLinksStampClock::OnlyTheAppProcessCanSay
        );
    }

    /// Every way of not knowing reads as no machine for a caller that has one
    /// thing to do with all three.
    #[test]
    fn only_a_named_machine_is_a_machine() {
        let this_machine = MachineClockIdentity::of_this_machine();

        assert_eq!(
            WhatIsKnownOfAnInboundLinksStampClock::TheMachine(this_machine)
                .the_machine_if_it_is_known(),
            Some(this_machine)
        );
        for not_known in [
            WhatIsKnownOfAnInboundLinksStampClock::NothingHasCrossedItYet,
            WhatIsKnownOfAnInboundLinksStampClock::OnlyTheAppProcessCanSay,
            WhatIsKnownOfAnInboundLinksStampClock::NoSuchLinkFeedsThatPort,
        ] {
            assert_eq!(not_known.the_machine_if_it_is_known(), None);
        }
    }
}
