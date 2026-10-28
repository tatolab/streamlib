// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A stream's location — where its code lives — and the stream id the runtime
//! computes from it, which keys and addresses the stream.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::graph::{EXPOSED_NAME_MAXIMUM_LENGTH, cast_exposed_name_to_url_safe};

/// Longest a stream id is, in characters: the exposed-name cast's bound.
pub(crate) const STREAM_ID_MAXIMUM_LENGTH: usize = EXPOSED_NAME_MAXIMUM_LENGTH;

/// How many hex digits of the location's SHA-256 a stream id carries.
const STREAM_ID_HASH_HEX_DIGITS: usize = 6;

/// What joins the folder, the function and the hash in a stream id.
const STREAM_ID_PART_SEPARATOR: char = '-';

/// Where a stream's code lives: its project directory, resolved, and its
/// stream function as `module:qualname`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamLocation {
    /// The project directory's absolute path, symlinks resolved.
    pub project_directory: PathBuf,
    /// The stream function as `module:qualname`; `stream.py:main` is `stream:main`.
    pub stream_function: String,
}

impl StreamLocation {
    /// The stream id of this location, `<folder>-<function>-<hash>`: the
    /// project directory's last component and the function's name, each
    /// cast, and the first six hex digits of SHA-256 over
    /// `<project_directory>\n<module:qualname>`. A part that casts to nothing
    /// is left out; folder and function are cut so the id fits the cast's 63
    /// characters, the hash always whole.
    pub fn stream_id(&self) -> String {
        let hash = self.location_hash_hex_prefix();
        let cast_parts: Vec<String> = [self.project_folder_name(), self.stream_function_name()]
            .into_iter()
            .filter_map(|part| {
                cast_exposed_name_to_url_safe(&part)
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

    /// The project directory's last component, empty when it has none.
    fn project_folder_name(&self) -> String {
        self.project_directory
            .file_name()
            .map(|folder_name| folder_name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The function's own name: the last segment of its qualname.
    fn stream_function_name(&self) -> String {
        let qualname = self
            .stream_function
            .rsplit_once(':')
            .map_or(self.stream_function.as_str(), |(_module, qualname)| {
                qualname
            });
        qualname
            .rsplit_once('.')
            .map_or(qualname, |(_enclosing, name)| name)
            .to_string()
    }

    fn location_hash_hex_prefix(&self) -> String {
        let mut location_hash = Sha256::new();
        location_hash.update(self.project_directory.to_string_lossy().as_bytes());
        location_hash.update(b"\n");
        location_hash.update(self.stream_function.as_bytes());
        let digest = location_hash.finalize();
        let mut hex: String = digest[..STREAM_ID_HASH_HEX_DIGITS.div_ceil(2)]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        hex.truncate(STREAM_ID_HASH_HEX_DIGITS);
        hex
    }
}

/// `<project directory>, <module:qualname>`, as a refusal names a location.
impl std::fmt::Display for StreamLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}, {}",
            self.project_directory.display(),
            self.stream_function
        )
    }
}

/// `cast_parts` — a folder and a function at most — cut so together they hold
/// at most `characters_left` ASCII characters: an even share each, the second
/// taking the odd one out, and a part shorter than its share leaving the rest to
/// the other. A part cut on the separator drops it.
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

    fn location(project_directory: &str, stream_function: &str) -> StreamLocation {
        StreamLocation {
            project_directory: PathBuf::from(project_directory),
            stream_function: stream_function.to_string(),
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
    fn the_id_is_the_cast_folder_and_function_and_six_hex_digits_of_the_locations_hash() {
        let hello = location("/home/someone/Hello World", "stream:main");

        assert_eq!(
            hello.stream_id(),
            format!(
                "hello-world-main-{}",
                sha256_hex_prefix_of("/home/someone/Hello World\nstream:main")
            )
        );
    }

    #[test]
    fn the_function_part_is_the_last_segment_of_the_qualname() {
        let nested = location("/srv/rover", "acme_rover.streams:Rigs.camera_rig");

        assert!(
            nested.stream_id().starts_with("rover-camera_rig-"),
            "{}",
            nested.stream_id()
        );
    }

    #[test]
    fn two_worktrees_of_one_project_get_different_ids_and_one_location_always_the_same() {
        let main_worktree = location("/home/someone/hello", "stream:main");
        let second_worktree = location("/home/someone/worktrees/hello", "stream:main");
        let other_function = location("/home/someone/hello", "stream:back");

        assert_ne!(main_worktree.stream_id(), second_worktree.stream_id());
        assert_ne!(main_worktree.stream_id(), other_function.stream_id());
        assert_eq!(
            main_worktree.stream_id(),
            location("/home/someone/hello", "stream:main").stream_id()
        );
    }

    #[test]
    fn a_part_that_casts_to_nothing_is_left_out() {
        let root = location("/", "stream:main");
        let unnamed_folder = location("/srv/日本", "stream:main");
        let nothing_left = location("/srv/日本", "stream:日本");

        let hash_of = |location: &StreamLocation| {
            sha256_hex_prefix_of(&format!(
                "{}\n{}",
                location.project_directory.display(),
                location.stream_function
            ))
        };
        assert_eq!(root.stream_id(), format!("main-{}", hash_of(&root)));
        assert_eq!(
            unnamed_folder.stream_id(),
            format!("main-{}", hash_of(&unnamed_folder))
        );
        assert_eq!(nothing_left.stream_id(), hash_of(&nothing_left));
    }

    #[test]
    fn a_long_folder_and_function_are_cut_to_fit_63_characters_with_the_hash_whole() {
        let long_folder = "f".repeat(80);
        let long_function = "g".repeat(80);
        let both_long = location(
            &format!("/srv/{long_folder}"),
            &format!("stream:{long_function}"),
        );
        let long_folder_short_function = location(&format!("/srv/{long_folder}"), "stream:main");

        let both_long_id = both_long.stream_id();
        assert_eq!(
            both_long_id.len(),
            STREAM_ID_MAXIMUM_LENGTH,
            "{both_long_id}"
        );
        assert_eq!(
            both_long_id,
            format!(
                "{}-{}-{}",
                "f".repeat(27),
                "g".repeat(28),
                both_long.location_hash_hex_prefix()
            )
        );
        let short_function_id = long_folder_short_function.stream_id();
        assert_eq!(
            short_function_id,
            format!(
                "{}-main-{}",
                "f".repeat(51),
                long_folder_short_function.location_hash_hex_prefix()
            )
        );
    }

    #[test]
    fn a_part_cut_at_a_separator_never_leaves_two_in_a_row() {
        let folder_cut_at_a_dash = format!("{}-{}", "a".repeat(26), "b".repeat(40));
        let cut = location(
            &format!("/srv/{folder_cut_at_a_dash}"),
            &format!("stream:{}", "g".repeat(80)),
        );

        let stream_id = cut.stream_id();

        assert!(!stream_id.contains("--"), "{stream_id}");
        assert!(
            stream_id.starts_with(&format!("{}-", "a".repeat(26))),
            "{stream_id}"
        );
    }

    #[test]
    fn every_id_is_already_in_cast_form() {
        for each in [
            location("/home/someone/Hello World", "stream:main"),
            location("/", "stream:日本"),
            location(&format!("/srv/{}", "x-".repeat(60)), "m:Ünïcödé"),
            location("/srv/.hidden", "pkg.mod:Outer.inner"),
        ] {
            let stream_id = each.stream_id();
            assert!(is_in_exposed_name_cast_form(&stream_id), "{stream_id}");
            assert!(stream_id.len() <= STREAM_ID_MAXIMUM_LENGTH, "{stream_id}");
        }
    }
}
