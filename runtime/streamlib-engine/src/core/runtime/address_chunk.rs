// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The grammar every part of a port's address obeys.
//!
//! A port is addressed `<runtime name>/<display name>/<port>`, so each part has
//! to be one chunk on its own: non-empty, free of the separator and the four
//! reserved characters, and not beginning with `@`.

/// The characters an address chunk may not contain: the separator `/`, and the
/// four the grammar reserves.
pub(crate) const CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN: [char; 5] = ['/', '*', '$', '#', '?'];

/// The character an address chunk may not begin with, which the grammar
/// reserves.
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

    /// Every character the grammar forbids reads back as the reason, by name.
    #[test]
    fn each_forbidden_character_is_named_as_the_reason() {
        for forbidden in CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN {
            let candidate = format!("camera{forbidden}one");
            let reason = first_reason_this_is_not_one_address_chunk(&candidate)
                .expect("a chunk carrying a forbidden character is not one chunk");
            assert!(
                reason.contains(&format!("{forbidden:?}")),
                "the reason {candidate:?} is not one chunk must name {forbidden:?}: {reason}"
            );
        }
    }

    /// A leading `@` is not one chunk; one anywhere else is.
    #[test]
    fn a_leading_at_sign_is_refused_and_an_inner_one_is_not() {
        let reason = first_reason_this_is_not_one_address_chunk("@runtime")
            .expect("a chunk beginning with '@' is not one chunk");
        assert!(reason.contains("begins with '@'"), "{reason}");
        assert!(is_one_legal_address_chunk("cam@home"));
    }

    /// An empty name reads as empty, rather than as some character.
    #[test]
    fn an_empty_name_reads_as_empty() {
        let reason = first_reason_this_is_not_one_address_chunk("")
            .expect("an empty chunk is not one chunk");
        assert!(reason.contains("it is empty"), "{reason}");
    }

    /// Spaces and unicode stay legal — a display name is free text otherwise.
    #[test]
    fn spaces_and_unicode_stay_legal() {
        for legal in ["slow sink", "こんにちは", "カメラ 2", "camera-1_a.b"] {
            assert!(
                is_one_legal_address_chunk(legal),
                "{legal:?} must stay a legal address chunk"
            );
        }
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

    /// The rule's table is exactly five forbidden characters and one forbidden
    /// leading character; widening or narrowing either renames what a stated
    /// runtime name may be.
    #[test]
    fn the_rules_table_is_the_separator_four_reserved_characters_and_a_leading_at_sign() {
        assert_eq!(
            CHARACTERS_NO_ADDRESS_CHUNK_MAY_CONTAIN,
            ['/', '*', '$', '#', '?']
        );
        assert_eq!(CHARACTER_NO_ADDRESS_CHUNK_MAY_BEGIN_WITH, '@');
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
    }

    /// Everything outside the table passes: punctuation the rule does not
    /// name, an inner `@`, spaces and unicode.
    #[test]
    fn every_name_outside_the_table_passes() {
        for legal in [
            "desk",
            "rig-desk-a1b2",
            "slow sink",
            "こんにちは",
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
