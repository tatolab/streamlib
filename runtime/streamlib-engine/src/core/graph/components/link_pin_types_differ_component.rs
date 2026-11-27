// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// That a link's two ends no longer carry types that connect, since an input choosing the type
/// of one of them changed: the link stays in the graph and carries nothing until the input is
/// changed back or the link is removed. Each pin type is in words, as a refusal names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkPinTypesDifferComponent {
    /// What the output the link leaves carries now.
    pub(crate) output_pin_type: String,
    /// What the input the link enters reads now.
    pub(crate) input_pin_type: String,
    /// Why the two do not connect, in the words of the verdict on them.
    pub(crate) why: String,
}
