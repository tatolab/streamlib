// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one recipe for a short id that is the same on every run of the same
//! thing: FNV-1a over the bytes that identify it, rendered base-36.
//!
//! FNV-1a rather than a `Hasher` from the standard library: `DefaultHasher`'s
//! output is explicitly not stable across Rust versions, and every caller here
//! needs a value that survives a rebuild.

/// The FNV-1a 64-bit offset basis.
const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// The FNV-1a 64-bit prime.
const FNV1A_64_PRIME: u64 = 0x0100_0000_01b3;

/// The alphabet a rendered id is drawn from.
const BASE36_ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// How many characters [`four_base36_characters_of`] renders.
const RENDERED_ID_CHARACTERS: usize = 4;

/// FNV-1a over `bytes`, the same value on every run and every Rust version.
pub fn fnv1a_64_hash_of(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV1A_64_OFFSET_BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV1A_64_PRIME)
    })
}

/// Four base-36 characters of `hash`, low digits first.
pub fn four_base36_characters_of(mut hash: u64) -> String {
    (0..RENDERED_ID_CHARACTERS)
        .map(|_| {
            let character = BASE36_ALPHABET[(hash % 36) as usize] as char;
            hash /= 36;
            character
        })
        .collect()
}

/// The short id naming whatever `bytes` identify — the two steps above, joined.
pub fn stable_short_id_over(bytes: &[u8]) -> String {
    four_base36_characters_of(fnv1a_64_hash_of(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recipe is pinned: these values are what every caller's ids are built
    /// from, so a change here renames every unnamed virtual camera and every
    /// unnamed runtime on the mesh.
    #[test]
    fn the_hash_is_the_published_fnv1a_64_of_the_bytes() {
        assert_eq!(fnv1a_64_hash_of(b""), FNV1A_64_OFFSET_BASIS);
        assert_eq!(fnv1a_64_hash_of(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64_hash_of(b"foobar"), 0x85944171f73967e8);
    }

    /// The same bytes render the same id, and different bytes differ.
    #[test]
    fn the_same_bytes_render_the_same_four_characters_every_time() {
        let one = stable_short_id_over(b"/home/someone/apps/desk");
        assert_eq!(one.chars().count(), RENDERED_ID_CHARACTERS);
        assert_eq!(one, stable_short_id_over(b"/home/someone/apps/desk"));
        assert_ne!(one, stable_short_id_over(b"/home/someone/apps/lab"));
    }

    /// Every rendered character is drawn from the base-36 alphabet.
    #[test]
    fn every_rendered_character_is_base36() {
        for seed in 0..64u64 {
            let rendered = four_base36_characters_of(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            assert!(
                rendered.bytes().all(|byte| BASE36_ALPHABET.contains(&byte)),
                "{rendered:?} left the alphabet"
            );
        }
    }
}
