// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The grammar every part of a port's mesh address obeys.
//!
//! A port is addressed `<runtime name>/<display name>/<port>`, so each part has
//! to be one legal Zenoh key chunk on its own.

/// The characters a key chunk may not contain: the separator itself and the
/// three the key-expression grammar reserves for matching.
pub(crate) const CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN: [char; 5] =
    ['/', '*', '$', '#', '?'];

/// The character a key chunk may not begin with. A leading `@` makes a chunk
/// verbatim, which `**` never matches, so an address carrying one would be
/// unreachable by any subscription over the mesh.
pub(crate) const CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH: char = '@';

/// Why `candidate` is not one legal chunk of a port's mesh address — `None` when
/// it is one, and otherwise the reason, named for a refusal.
///
/// One chunk is non-empty, free of [`CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN`],
/// and does not begin with [`CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH`].
/// Spaces and unicode are legal, as they are in a display name today.
pub(crate) fn first_reason_this_is_not_one_mesh_address_chunk(candidate: &str) -> Option<String> {
    if candidate.is_empty() {
        return Some("it is empty".to_string());
    }
    if let Some(refused_character) = candidate
        .chars()
        .find(|character| CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN.contains(character))
    {
        return Some(format!("it contains {refused_character:?}"));
    }
    if candidate.starts_with(CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH) {
        return Some(format!(
            "it begins with {CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH:?}"
        ));
    }
    None
}

/// The sentence every refusal ends with, so both callers state the same rule.
pub fn what_one_mesh_address_chunk_may_be() -> String {
    let listed = CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN
        .iter()
        .map(|character| format!("'{character}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "A port is addressed <runtime name>/<display name>/<port> on the runtime mesh, so each \
         part is one key chunk: non-empty, containing none of {listed}, and not beginning with \
         '{CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH}'"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_one_legal_mesh_address_chunk(candidate: &str) -> bool {
        first_reason_this_is_not_one_mesh_address_chunk(candidate).is_none()
    }

    /// Every character the grammar forbids reads back as the reason, by name.
    #[test]
    fn each_forbidden_character_is_named_as_the_reason() {
        for forbidden in CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN {
            let candidate = format!("camera{forbidden}one");
            let reason = first_reason_this_is_not_one_mesh_address_chunk(&candidate)
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
        let reason = first_reason_this_is_not_one_mesh_address_chunk("@runtime")
            .expect("a chunk beginning with '@' is not one chunk");
        assert!(reason.contains("begins with '@'"), "{reason}");
        assert!(is_one_legal_mesh_address_chunk("cam@home"));
    }

    /// An empty name reads as empty, rather than as some character.
    #[test]
    fn an_empty_name_reads_as_empty() {
        let reason = first_reason_this_is_not_one_mesh_address_chunk("")
            .expect("an empty chunk is not one chunk");
        assert!(reason.contains("it is empty"), "{reason}");
    }

    /// Spaces and unicode stay legal — a display name is free text otherwise.
    #[test]
    fn spaces_and_unicode_stay_legal() {
        for legal in ["slow sink", "こんにちは", "カメラ 2", "camera-1_a.b"] {
            assert!(
                is_one_legal_mesh_address_chunk(legal),
                "{legal:?} must stay a legal mesh address chunk"
            );
        }
    }

    /// The engine's own disambiguation suffix never produces a refused name.
    #[test]
    fn the_engines_disambiguation_suffix_always_passes() {
        for ordinal in 2..=11 {
            assert!(is_one_legal_mesh_address_chunk(&format!(
                "CameraSource {ordinal}"
            )));
        }
    }

    /// The grammar is graded by Zenoh rather than by a second reading of the
    /// spec: a name this module accepts is exactly a name `zenoh-keyexpr` reads
    /// as one literal chunk a `**` subscription reaches.
    #[test]
    fn the_grammar_agrees_with_zenohs_own_key_expression_rules() {
        use zenoh_keyexpr::keyexpr;

        let every_key = keyexpr::new("**").expect("`**` is a key expression");
        let zenoh_reads_it_as_one_reachable_literal_chunk = |candidate: &str| {
            keyexpr::new(candidate).is_ok_and(|key| {
                key.chunks().count() == 1 && !key.is_wild() && every_key.includes(key)
            })
        };

        let mut candidates = vec![
            "desk".to_string(),
            "rig-desk-a1b2".to_string(),
            "slow sink".to_string(),
            "こんにちは".to_string(),
            "cam@home".to_string(),
            "CameraSource 2".to_string(),
            "@runtime".to_string(),
            "@".to_string(),
            String::new(),
        ];
        for forbidden in CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN {
            candidates.push(format!("desk{forbidden}rig"));
            candidates.push(forbidden.to_string());
        }

        for candidate in candidates {
            assert_eq!(
                is_one_legal_mesh_address_chunk(&candidate),
                zenoh_reads_it_as_one_reachable_literal_chunk(&candidate),
                "the engine and zenoh-keyexpr disagree about {candidate:?}"
            );
        }
    }
}
