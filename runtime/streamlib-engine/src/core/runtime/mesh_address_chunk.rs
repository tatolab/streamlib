// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The grammar every part of a port's mesh address obeys.
//!
//! A port is addressed `<runtime name>/<display name>/<port>`, so each part has
//! to be one legal Zenoh key chunk on its own.

use crate::core::error::{Error, Result};

/// The characters a key chunk may not contain: the separator itself and the
/// three the key-expression grammar reserves for matching.
pub const CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN: [char; 5] = ['/', '*', '$', '#', '?'];

/// The character a key chunk may not begin with. A leading `@` makes a chunk
/// verbatim, which `**` never matches, so an address carrying one would be
/// unreachable by any subscription over the mesh.
pub const CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH: char = '@';

/// Whether `candidate` is one legal chunk of a port's mesh address.
///
/// Non-empty, free of [`CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN`], and not
/// beginning with [`CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH`]. Spaces and
/// unicode are legal, as they are in a display name today.
pub fn is_one_legal_mesh_address_chunk(candidate: &str) -> bool {
    first_reason_this_is_not_one_mesh_address_chunk(candidate).is_none()
}

/// Why `candidate` is not one legal mesh address chunk, named for a refusal.
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
pub(crate) fn what_one_mesh_address_chunk_may_be() -> String {
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

/// Refuse `requested_display_name` unless it is one legal mesh address chunk.
pub(crate) fn refuse_a_display_name_that_is_not_one_mesh_address_chunk(
    requested_display_name: &str,
) -> Result<()> {
    match first_reason_this_is_not_one_mesh_address_chunk(requested_display_name) {
        None => Ok(()),
        Some(what_is_wrong) => Err(Error::Configuration(format!(
            "display name {requested_display_name:?} cannot be one chunk of a processor's mesh \
             address: {what_is_wrong}. {}. Rename the processor, or leave `display_name` out to \
             take the class's own short name",
            what_one_mesh_address_chunk_may_be()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character the grammar forbids is refused, named in the refusal.
    #[test]
    fn each_forbidden_character_is_refused_by_name() {
        for forbidden in CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN {
            let candidate = format!("camera{forbidden}one");
            let refusal = refuse_a_display_name_that_is_not_one_mesh_address_chunk(&candidate)
                .expect_err("a chunk carrying a forbidden character must be refused");
            assert!(
                refusal.to_string().contains(&format!("{forbidden:?}")),
                "the refusal of {candidate:?} must name {forbidden:?}: {refusal}"
            );
        }
    }

    /// A leading `@` is refused; one anywhere else is not.
    #[test]
    fn a_leading_at_sign_is_refused_and_an_inner_one_is_not() {
        let refusal = refuse_a_display_name_that_is_not_one_mesh_address_chunk("@runtime")
            .expect_err("a chunk beginning with '@' must be refused");
        assert!(
            refusal.to_string().contains("begins with '@'"),
            "the refusal must say what is wrong: {refusal}"
        );
        assert!(is_one_legal_mesh_address_chunk("cam@home"));
    }

    /// An empty name is refused saying so, rather than by some character.
    #[test]
    fn an_empty_name_is_refused_saying_it_is_empty() {
        let refusal = refuse_a_display_name_that_is_not_one_mesh_address_chunk("")
            .expect_err("an empty chunk must be refused");
        assert!(
            refusal.to_string().contains("it is empty"),
            "the refusal must say the name was empty: {refusal}"
        );
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
