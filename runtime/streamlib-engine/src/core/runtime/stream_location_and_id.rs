// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's location — its project and its stream asset, by UUID, and the directory the
//! project sits in — and the stream id the runtime computes for it, which keys and addresses
//! the stream.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::core::graph::{EXPOSED_NAME_MAXIMUM_LENGTH, cast_exposed_name_to_url_safe};

/// Longest a stream id is, in characters: the exposed-name cast's bound.
pub(crate) const STREAM_ID_MAXIMUM_LENGTH: usize = EXPOSED_NAME_MAXIMUM_LENGTH;

/// How many hex digits of the SHA-256 over the two UUIDs a stream id carries.
const STREAM_ID_HASH_HEX_DIGITS: usize = 6;

/// What joins the project's name, the stream's name and the hash in a stream id.
const STREAM_ID_PART_SEPARATOR: char = '-';

/// Where a stream lives: its project's UUID and its stream asset's UUID, which identify it,
/// and the directory the project sat in at its last load.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamLocation {
    /// The project directory's absolute path, symlinks resolved, as of the stream's last load.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: Uuid,
    /// The stream asset's UUID, from its graph document.
    pub stream_asset: Uuid,
}

impl StreamLocation {
    /// Whether `other` locates the same stream: the same project and stream asset, wherever
    /// the project sits.
    pub fn is_the_same_stream_as(&self, other: &StreamLocation) -> bool {
        self.project == other.project && self.stream_asset == other.stream_asset
    }

    /// The stream id this location is first recorded under, `<project>-<stream>-<hash>`: the
    /// project's name and the stream's name, each cast, and the first six hex digits of
    /// SHA-256 over `<project uuid>\n<stream asset uuid>`. A part that casts to nothing is left
    /// out; the names are cut so the id fits the cast's 63 characters, the hash always whole.
    pub fn stream_id_named(&self, project_name: &str, stream_name: &str) -> String {
        let hash = self.location_hash_hex_prefix();
        let cast_parts: Vec<String> = [project_name, stream_name]
            .into_iter()
            .filter_map(|part| {
                cast_exposed_name_to_url_safe(part)
                    .ok()
                    .map(|cast| cast.into_owned())
            })
            .collect();
        let characters_left_for_the_parts =
            STREAM_ID_MAXIMUM_LENGTH - STREAM_ID_HASH_HEX_DIGITS - cast_parts.len();
        let mut stream_id = String::with_capacity(STREAM_ID_MAXIMUM_LENGTH);
        for part in parts_cut_to_share(cast_parts, characters_left_for_the_parts) {
            stream_id.push_str(&part);
            stream_id.push(STREAM_ID_PART_SEPARATOR);
        }
        stream_id.push_str(&hash);
        stream_id
    }

    /// The first [`STREAM_ID_HASH_HEX_DIGITS`] hex digits of SHA-256 over
    /// `<project uuid>\n<stream asset uuid>`.
    fn location_hash_hex_prefix(&self) -> String {
        let mut location_hash = Sha256::new();
        location_hash.update(self.project.to_string().as_bytes());
        location_hash.update(b"\n");
        location_hash.update(self.stream_asset.to_string().as_bytes());
        let digest = location_hash.finalize();
        let mut hex: String = digest[..STREAM_ID_HASH_HEX_DIGITS.div_ceil(2)]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        hex.truncate(STREAM_ID_HASH_HEX_DIGITS);
        hex
    }
}

impl std::fmt::Display for StreamLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the stream asset {} of the project {} in {}",
            self.stream_asset,
            self.project,
            self.project_directory.display()
        )
    }
}

/// `cast_parts` cut to share `characters_left`: a part shorter than its even share keeps it
/// whole and leaves the rest to the other; a cut never ends a part on a separator.
fn parts_cut_to_share(cast_parts: Vec<String>, characters_left: usize) -> Vec<String> {
    let lengths: Vec<usize> = cast_parts.iter().map(String::len).collect();
    if lengths.iter().sum::<usize>() <= characters_left {
        return cast_parts;
    }
    let shares: Vec<usize> = match lengths.as_slice() {
        [_only] => vec![characters_left],
        [first, second] => {
            let first_even_share = characters_left / 2;
            let second_even_share = characters_left - first_even_share;
            if *first <= first_even_share {
                vec![*first, characters_left - first]
            } else if *second <= second_even_share {
                vec![characters_left - second, *second]
            } else {
                vec![first_even_share, second_even_share]
            }
        }
        _ => lengths,
    };
    cast_parts
        .into_iter()
        .zip(shares)
        .map(|(mut part, share)| {
            // Every character of a cast part is ASCII, so a byte truncate cuts on a character.
            part.truncate(share);
            while part.ends_with(STREAM_ID_PART_SEPARATOR) {
                part.pop();
            }
            part
        })
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::is_in_exposed_name_cast_form;

    fn location() -> StreamLocation {
        StreamLocation {
            project_directory: PathBuf::from("/home/someone/hello"),
            project: Uuid::new_v4(),
            stream_asset: Uuid::new_v4(),
        }
    }

    fn sha256_hex_prefix_of(text: &str) -> String {
        let digest = Sha256::digest(text.as_bytes());
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..STREAM_ID_HASH_HEX_DIGITS]
            .to_string()
    }

    #[test]
    fn the_id_is_the_cast_names_and_six_hex_digits_of_the_hash_over_the_two_uuids() {
        let hello = location();

        assert_eq!(
            hello.stream_id_named("Hello World", "main"),
            format!(
                "hello-world-main-{}",
                sha256_hex_prefix_of(&format!("{}\n{}", hello.project, hello.stream_asset))
            )
        );
    }

    #[test]
    fn a_moved_project_keeps_the_hash_and_another_stream_asset_does_not() {
        let at_home = location();
        let moved = StreamLocation {
            project_directory: PathBuf::from("/srv/elsewhere"),
            ..at_home.clone()
        };
        let another_stream = StreamLocation {
            stream_asset: Uuid::new_v4(),
            ..at_home.clone()
        };

        assert!(moved.is_the_same_stream_as(&at_home));
        assert_eq!(
            moved.stream_id_named("hello", "main"),
            at_home.stream_id_named("hello", "main")
        );
        assert!(!another_stream.is_the_same_stream_as(&at_home));
        assert_ne!(
            another_stream.stream_id_named("hello", "main"),
            at_home.stream_id_named("hello", "main")
        );
    }

    #[test]
    fn a_part_that_casts_to_nothing_is_left_out() {
        let each = location();
        let hash = each.location_hash_hex_prefix();

        assert_eq!(each.stream_id_named("", "main"), format!("main-{hash}"));
        assert_eq!(each.stream_id_named("日本", "main"), format!("main-{hash}"));
        assert_eq!(each.stream_id_named("日本", "日本"), hash);
    }

    #[test]
    fn long_names_are_cut_to_fit_63_characters_with_the_hash_whole() {
        let each = location();
        let hash = each.location_hash_hex_prefix();

        let both_long_id = each.stream_id_named(&"f".repeat(80), &"g".repeat(80));
        assert_eq!(
            both_long_id.len(),
            STREAM_ID_MAXIMUM_LENGTH,
            "{both_long_id}"
        );
        assert_eq!(
            both_long_id,
            format!("{}-{}-{hash}", "f".repeat(27), "g".repeat(28))
        );
        assert_eq!(
            each.stream_id_named(&"f".repeat(80), "main"),
            format!("{}-main-{hash}", "f".repeat(51))
        );
    }

    #[test]
    fn a_part_cut_at_a_separator_never_leaves_two_in_a_row() {
        let stream_id = location().stream_id_named(
            &format!("{}-{}", "a".repeat(26), "b".repeat(40)),
            &"g".repeat(80),
        );

        assert!(!stream_id.contains("--"), "{stream_id}");
        assert!(
            stream_id.starts_with(&format!("{}-", "a".repeat(26))),
            "{stream_id}"
        );
    }

    #[test]
    fn every_id_is_already_in_cast_form() {
        let each = location();
        for (project_name, stream_name) in [
            ("Hello World", "main"),
            ("", "日本"),
            (&*"x-".repeat(60), "Ünïcödé"),
            (".hidden", "Outer.inner"),
        ] {
            let stream_id = each.stream_id_named(project_name, stream_name);
            assert!(is_in_exposed_name_cast_form(&stream_id), "{stream_id}");
            assert!(stream_id.len() <= STREAM_ID_MAXIMUM_LENGTH, "{stream_id}");
        }
    }
}
