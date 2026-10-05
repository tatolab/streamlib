// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The grammar every part of a port's address obeys.
//!
//! A port is addressed `<runtime name>/<display name>/<port>`, so each part has
//! to be one chunk on its own.

/// The characters an address chunk may not contain.
pub(crate) const CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN: [char; 5] = ['/', '*', '$', '#', '?'];

/// The character an address chunk may not begin with.
pub(crate) const CHARACTER_NO_ADDRESS_CHUNK_MAY_BEGIN_WITH: char = '@';

/// Why `candidate` is not one legal chunk of a port's address — `None` when it
/// is one, and otherwise the reason, named for a refusal.
///
/// One chunk is non-empty, free of [`CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN`],
/// and does not begin with [`CHARACTER_NO_ADDRESS_CHUNK_MAY_BEGIN_WITH`].
/// Spaces and unicode are legal, as they are in a display name today.
pub(crate) fn first_reason_this_is_not_one_address_chunk(candidate: &str) -> Option<String> {
    if candidate.is_empty() {
        return Some("it is empty".to_string());
    }
    if let Some(refused_character) = candidate
        .chars()
        .find(|character| CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN.contains(character))
    {
        return Some(format!("it contains {refused_character:?}"));
    }
    if candidate.starts_with(CHARACTER_NO_ADDRESS_CHUNK_MAY_BEGIN_WITH) {
        return Some(format!(
            "it begins with {CHARACTER_NO_ADDRESS_CHUNK_MAY_BEGIN_WITH:?}"
        ));
    }
    None
}

/// The sentence every refusal ends with, so both callers state the same rule.
pub fn what_one_address_chunk_may_be() -> String {
    let listed = CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN
        .iter()
        .map(|character| format!("'{character}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "A port is addressed <runtime name>/<display name>/<port>, so each part is one \
         address chunk: non-empty, containing none of {listed}, and not beginning with \
         '{CHARACTER_NO_ADDRESS_CHUNK_MAY_BEGIN_WITH}'"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_one_legal_address_chunk(candidate: &str) -> bool {
        first_reason_this_is_not_one_address_chunk(candidate).is_none()
    }

    /// A defaulted node name and its `-2`, `-3` … suffix never produce a
    /// refused chunk.
    #[test]
    fn the_engines_defaulted_names_always_pass() {
        for ordinal in 2..=11 {
            assert!(is_one_legal_address_chunk(&format!(
                "camerasource-{ordinal}"
            )));
        }
    }

    /// Each refused shape reads back with the reason the rule gives it, the
    /// forbidden characters checked before the leading `@`.
    #[test]
    fn every_refused_shape_in_the_table_reads_back_with_its_reason() {
        let refused_with_the_reason = [
            ("", "it is empty"),
            ("@", "it begins with '@'"),
            ("@runtime", "it begins with '@'"),
            ("/", "it contains '/'"),
            ("desk/rig", "it contains '/'"),
            ("*", "it contains '*'"),
            ("desk*rig", "it contains '*'"),
            ("$", "it contains '$'"),
            ("desk$rig", "it contains '$'"),
            ("#", "it contains '#'"),
            ("desk#rig", "it contains '#'"),
            ("?", "it contains '?'"),
            ("desk?rig", "it contains '?'"),
            ("@desk/rig", "it contains '/'"),
        ];
        for (candidate, expected_reason) in refused_with_the_reason {
            assert_eq!(
                first_reason_this_is_not_one_address_chunk(candidate).as_deref(),
                Some(expected_reason),
                "{candidate:?}"
            );
        }
        for forbidden in CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN {
            assert_eq!(
                first_reason_this_is_not_one_address_chunk(&format!("desk{forbidden}rig")),
                Some(format!("it contains {forbidden:?}")),
            );
        }
    }

    /// Everything outside the table passes: punctuation the rule does not
    /// name, an inner `@`, spaces and unicode.
    #[test]
    fn names_with_unlisted_punctuation_an_inner_at_sign_spaces_and_unicode_pass() {
        for legal in [
            "desk",
            "rig-desk-a1b2",
            "slow sink",
            "こんにちは",
            "カメラ 2",
            "cam@home",
            "CameraSource 2",
            "camera-1_a.b",
            "desk:rig",
            "desk%rig",
            "desk.rig~1",
            "desk+rig=1",
        ] {
            assert!(
                is_one_legal_address_chunk(legal),
                "{legal:?} must be one legal address chunk"
            );
        }
    }
}
