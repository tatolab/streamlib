// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What one end of a link states against what the other holds or takes: formats, codecs, media,
//! Menu options and names.

/// Each of `stated` that `held` does not hold, in `stated`'s order.
pub(crate) fn the_ones_not_held<'stated, T: PartialEq>(
    stated: &'stated [T],
    held: &[T],
) -> Vec<&'stated T> {
    stated.iter().filter(|one| !held.contains(one)).collect()
}

/// Each of `stated` that `taken` does not take, in `stated`'s order; none when `taken` is empty,
/// as an input stating none takes any.
pub(crate) fn the_ones_not_taken<'stated, T: PartialEq>(
    stated: &'stated [T],
    taken: &[T],
) -> Vec<&'stated T> {
    if taken.is_empty() {
        return Vec::new();
    }
    the_ones_not_held(stated, taken)
}
