// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The scope's refusals: a `.get()` that read no value, and a use of the API that can never be
//! right.

use std::fmt;
use std::panic::resume_unwind;

use crate::BuildFailure;

/// Why a `.get()` read no value, for a binding that cannot let a panic cross its caller's frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadRefused {
    /// The read ran outside a build or a frame.
    OutsideABuildOrAFrame,
    /// The read ran in the node's body, which runs once and would never see the value change.
    InTheBody,
    /// The handle belongs to another scope than the build or frame running on this thread.
    HandleOfAnotherScope,
    /// A build read a computed value that has no value yet; the read is recorded, so
    /// [`fail_this_build_on_a_computed_value_not_yet_built`] makes it build again once that value
    /// is built.
    ComputedValueNotYetBuilt,
}

impl ReadRefused {
    /// The refusal a panicking `.get()` raises for it; none for a value not yet built, which fails
    /// the build instead.
    pub(crate) fn refusal(self) -> Option<Refusal> {
        match self {
            ReadRefused::OutsideABuildOrAFrame => Some(Refusal::GetOutsideABuildOrAFrame),
            ReadRefused::InTheBody => Some(Refusal::GetInTheBody),
            ReadRefused::HandleOfAnotherScope => Some(Refusal::HandleOfAnotherScope),
            ReadRefused::ComputedValueNotYetBuilt => None,
        }
    }
}

impl fmt::Display for ReadRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.refusal() {
            Some(refusal) => refusal.fmt(f),
            None => BuildFailure::ReadAComputedValueNotYetBuilt.fmt(f),
        }
    }
}

impl std::error::Error for ReadRefused {}

/// End the build running on this thread as [`BuildFailure::ReadAComputedValueNotYetBuilt`], for
/// a binding whose build saw [`ReadRefused::ComputedValueNotYetBuilt`] and returned to Rust.
pub fn fail_this_build_on_a_computed_value_not_yet_built() -> ! {
    resume_unwind(Box::new(ReadOfAComputedValueNotYetBuilt))
}

/// A use of the scope's API that can never be right, refused with a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    GetOutsideABuildOrAFrame,
    GetInTheBody,
    ComputedOutsideADeclaration,
    HandleOfAnotherScope,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::GetOutsideABuildOrAFrame => {
                "`.get()` reads only in a build or a frame; read the value in a `computed` build \
                 or in the per-frame function"
            }
            Refusal::GetInTheBody => {
                "`.get()` can't read in the node's body, which runs once and would never see the \
                 value change; read it in a `computed` build or in the per-frame function"
            }
            Refusal::ComputedOutsideADeclaration => {
                "`computed` declares only while a scope is being declared, in the node's body"
            }
            Refusal::HandleOfAnotherScope => "the handle belongs to another scope",
        })
    }
}

pub(crate) fn refuse(refusal: Refusal) -> ! {
    panic!("{refusal}")
}

/// The unwind payload of a build's `.get()` of a computed value that has no value yet.
pub(crate) struct ReadOfAComputedValueNotYetBuilt;

pub(crate) fn refuse_the_read(refused: ReadRefused) -> ! {
    match refused.refusal() {
        Some(refusal) => refuse(refusal),
        None => fail_this_build_on_a_computed_value_not_yet_built(),
    }
}
