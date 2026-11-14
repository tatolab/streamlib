// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The published frame id grammar, `<pool slot>#<frame generation>`: one id
//! per publish over a pool slot, keyed back to the slot by caches that import
//! a slot once.

/// The building half of the grammar: the surface id `frame_generation`
/// publishes over `pool_slot`.
pub fn published_frame_id_of_pool_slot(pool_slot: &str, frame_generation: u64) -> String {
    format!("{pool_slot}#{frame_generation}")
}

/// The borrowing half of the grammar: the slot and generation of a published
/// frame id, or `None` for an id that carries no `#<generation>` suffix.
pub fn split_pool_slot_and_frame_generation(surface_id: &str) -> Option<(&str, u64)> {
    let (pool_slot, generation_digits) = surface_id.rsplit_once('#')?;
    if pool_slot.is_empty()
        || generation_digits.is_empty()
        || !generation_digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((pool_slot, generation_digits.parse().ok()?))
}

/// The pool-slot portion of any surface id: strips a published frame id's
/// `#<generation>` suffix and returns every other id whole. What per-slot
/// caches normalize their keys through.
pub fn pool_slot_key_of_surface_id(surface_id: &str) -> &str {
    match split_pool_slot_and_frame_generation(surface_id) {
        Some((pool_slot, _)) => pool_slot,
        None => surface_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_publish_over_one_pool_slot_keys_back_to_that_slot() {
        let first_publish = published_frame_id_of_pool_slot("camera-pool-slot-2", 7);
        let next_publish = published_frame_id_of_pool_slot("camera-pool-slot-2", 8);

        assert_ne!(first_publish, next_publish);
        assert_eq!(
            pool_slot_key_of_surface_id(&first_publish),
            "camera-pool-slot-2"
        );
        assert_eq!(
            pool_slot_key_of_surface_id(&next_publish),
            "camera-pool-slot-2"
        );
        assert_eq!(
            split_pool_slot_and_frame_generation(&next_publish),
            Some(("camera-pool-slot-2", 8))
        );
    }

    #[test]
    fn an_id_without_a_generation_is_its_own_key() {
        for unpublished_id in ["registered-surface", "slot#", "#4", "slot#4a"] {
            assert_eq!(pool_slot_key_of_surface_id(unpublished_id), unpublished_id);
            assert_eq!(split_pool_slot_and_frame_generation(unpublished_id), None);
        }
    }
}
