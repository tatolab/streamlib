// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which machine's monotonic clock the bags arriving on one inbound link were
//! stamped on.
//!
//! One clock per machine ([`MachineClockIdentity`]), so a destination fanning
//! several links in has to be able to ask, link by link, which one it is
//! reading before it compares two stamps. The answer belongs to the link rather
//! than to each bag (owner, 2026-09-14).
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

/// The token a far side is wired with for a link whose bags were stamped on
/// this machine.
pub const THIS_MACHINE_STAMP_CLOCK_TOKEN: &str = "this_machine";

/// The token a far side is wired with for a link only the app process can name
/// the machine of.
pub const ONLY_THE_APP_PROCESS_CAN_NAME_STAMP_CLOCK_TOKEN: &str =
    "a_machine_only_the_app_process_can_name";

impl TheClockAnInboundLinksStampsAreTakenOn {
    /// Which of the two answers a far side is wired with, as the envelope
    /// spells it.
    ///
    /// The cell cannot cross a process boundary, so what a helper is told is
    /// the *kind* of answer rather than the answer: this machine, or a machine
    /// only the app process holds a mesh session to name. Sent rather than
    /// re-derived on the far side, because this side already knows it from the
    /// link's source and a far side guessing from the shape of two names would
    /// guess wrong the moment either name changed.
    pub fn as_the_token_a_far_side_is_wired_with(&self) -> &'static str {
        match self {
            Self::ThisMachine => THIS_MACHINE_STAMP_CLOCK_TOKEN,
            Self::WhicheverMachineTheMeshIsCarryingFrom(_)
            | Self::AMachineOnlyTheAppProcessCanName => {
                ONLY_THE_APP_PROCESS_CAN_NAME_STAMP_CLOCK_TOKEN
            }
        }
    }

    /// The answer a far side reads off the token it was wired with, or `None`
    /// for a token this build does not know.
    pub fn of_the_token_a_far_side_was_wired_with(token: &str) -> Option<Self> {
        match token {
            THIS_MACHINE_STAMP_CLOCK_TOKEN => Some(Self::ThisMachine),
            ONLY_THE_APP_PROCESS_CAN_NAME_STAMP_CLOCK_TOKEN => {
                Some(Self::AMachineOnlyTheAppProcessCanName)
            }
            _ => None,
        }
    }

    /// What this process can say about the clock right now.
    pub fn what_is_known_of_it(&self) -> WhatIsKnownOfAnInboundLinksStampClock {
        match self {
            Self::ThisMachine => Some(MachineClockIdentity::of_this_machine()).into(),
            Self::WhicheverMachineTheMeshIsCarryingFrom(carries_from) => {
                carries_from.what_it_is_now().into()
            }
            Self::AMachineOnlyTheAppProcessCanName => {
                WhatIsKnownOfAnInboundLinksStampClock::OnlyTheAppProcessCanSay
            }
        }
    }
}

/// The answer to "which machine's clock are this link's stamps taken on".
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
    /// The machine its bags were stamped on names no clock of its own — a
    /// platform with no boot-session id to report.
    ///
    /// Never [`TheMachine`] carrying the nil id: two machines that each name no
    /// clock are not one machine, and an identity two of them share is one a
    /// reader would compare stamps across.
    ///
    /// [`TheMachine`]: Self::TheMachine
    ItsMachineNamesNoClockOfItsOwn,
    /// Nothing of that name is bound on that port here — no such link, or a
    /// process that holds no binding for it at all.
    NoSuchLinkFeedsThatPort,
}

impl From<Option<MachineClockIdentity>> for WhatIsKnownOfAnInboundLinksStampClock {
    /// What a cell's current reading means, in one place: both the binding and
    /// the ingress table answer off one of these, and the two must agree on
    /// what an empty cell says.
    fn from(what_a_cell_reads: Option<MachineClockIdentity>) -> Self {
        match what_a_cell_reads {
            Some(machine) if machine.is_unidentified() => Self::ItsMachineNamesNoClockOfItsOwn,
            Some(machine) => Self::TheMachine(machine),
            None => Self::NothingHasCrossedItYet,
        }
    }
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
            | Self::ItsMachineNamesNoClockOfItsOwn
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

    /// A link's kind survives the envelope, and a remote one lands on the far
    /// side as the answer only the app process can give — never as this
    /// machine, which is the answer that would let two clocks be compared.
    #[test]
    fn every_clocks_token_reads_back_as_the_answer_a_far_side_owes() {
        let carries_from = Arc::new(MachineClockARemoteLinkCarriesFrom::default());

        for (wired_with, what_the_far_side_reads) in [
            (
                TheClockAnInboundLinksStampsAreTakenOn::ThisMachine,
                WhatIsKnownOfAnInboundLinksStampClock::TheMachine(
                    MachineClockIdentity::of_this_machine(),
                ),
            ),
            (
                TheClockAnInboundLinksStampsAreTakenOn::WhicheverMachineTheMeshIsCarryingFrom(
                    Arc::clone(&carries_from),
                ),
                WhatIsKnownOfAnInboundLinksStampClock::OnlyTheAppProcessCanSay,
            ),
        ] {
            let token = wired_with.as_the_token_a_far_side_is_wired_with();
            let on_the_far_side =
                TheClockAnInboundLinksStampsAreTakenOn::of_the_token_a_far_side_was_wired_with(
                    token,
                )
                .unwrap_or_else(|| panic!("{token} must read back as an answer"));

            assert_eq!(
                on_the_far_side.what_is_known_of_it(),
                what_the_far_side_reads
            );
        }
    }

    /// A token this build does not know is read as nothing, so a far side
    /// refuses the wiring by name rather than falling back to this machine.
    #[test]
    fn a_token_this_build_does_not_know_reads_as_no_answer() {
        for not_one in ["", "this machine", "THIS_MACHINE", "another_machine"] {
            assert!(
                TheClockAnInboundLinksStampsAreTakenOn::of_the_token_a_far_side_was_wired_with(
                    not_one
                )
                .is_none(),
                "{not_one:?} must read as no answer"
            );
        }
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

    /// A platform that names no clock is not a machine two links can share.
    ///
    /// Fail-without-fix: read the nil id as an identity and two tracks from two
    /// such machines compare equal — the epoch-mixing this whole surface
    /// exists to stop, reached on any platform that is neither Linux nor Apple
    /// and on a Linux box whose boot id does not answer.
    #[test]
    fn a_machine_that_names_no_clock_is_never_one_two_links_can_share() {
        let carries_from = Arc::new(MachineClockARemoteLinkCarriesFrom::default());
        carries_from.note_the_machine_a_bag_was_stamped_on(MachineClockIdentity::UNIDENTIFIED);

        let what_is_known =
            TheClockAnInboundLinksStampsAreTakenOn::WhicheverMachineTheMeshIsCarryingFrom(
                carries_from,
            )
            .what_is_known_of_it();

        assert_eq!(
            what_is_known,
            WhatIsKnownOfAnInboundLinksStampClock::ItsMachineNamesNoClockOfItsOwn
        );
        assert_eq!(
            what_is_known.the_machine_if_it_is_known(),
            None,
            "nothing may be compared against a machine that named no clock"
        );
    }

    /// Every way of not knowing reads as no machine for a caller that has one
    /// thing to do with all four.
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
            WhatIsKnownOfAnInboundLinksStampClock::ItsMachineNamesNoClockOfItsOwn,
            WhatIsKnownOfAnInboundLinksStampClock::NoSuchLinkFeedsThatPort,
        ] {
            assert_eq!(not_known.the_machine_if_it_is_known(), None);
        }
    }
}
