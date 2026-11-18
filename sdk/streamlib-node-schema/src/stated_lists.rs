// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What one end of a link states against what the other holds or takes: formats, codecs, media,
//! Menu options and names.

/// Each of `stated` that `held` does not hold, in `stated`'s order.
pub(crate) fn the_ones_not_held<'list, T: PartialEq>(
    stated: &'list [T],
    held: &'list [T],
) -> impl Iterator<Item = &'list T> {
    stated.iter().filter(move |one| !held.contains(one))
}

/// Each of `stated` that `taken` does not take, in `stated`'s order; none when `taken` is empty,
/// as an input stating none takes any.
pub(crate) fn the_ones_not_taken<'list, T: PartialEq>(
    stated: &'list [T],
    taken: &'list [T],
) -> impl Iterator<Item = &'list T> {
    let stated = if taken.is_empty() { &[][..] } else { stated };
    the_ones_not_held(stated, taken)
}
