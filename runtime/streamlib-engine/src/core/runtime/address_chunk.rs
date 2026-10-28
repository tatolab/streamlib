// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The grammar every chunk of an address obeys.
//!
//! An address is `<machine>/<stream id>/<node>/<port>`, and a chunk is a name the
//! exposed-name cast leaves as it is and does not refuse. Every other grammar
//! that holds a chunk — a channel name's — defers here.

use crate::core::graph::{
    EXPOSED_NAME_MAXIMUM_LENGTH, cast_exposed_name_to_url_safe, is_in_exposed_name_cast_form,
};

/// Why `candidate` is not one chunk of an address — `None` when it is one, and
/// otherwise the reason, named for a refusal.
pub(crate) fn first_reason_this_is_not_one_address_chunk(candidate: &str) -> Option<String> {
    if is_in_exposed_name_cast_form(candidate) {
        return None;
    }
    if candidate.is_empty() {
        return Some("it is empty".to_string());
    }
    Some(match cast_exposed_name_to_url_safe(candidate) {
        Ok(cast) => format!("it is not in cast form, which would make it `{cast}`"),
        Err(casts_to_nothing) => casts_to_nothing.to_string(),
    })
}

/// The sentence every refusal of a chunk ends with, so every caller states the
/// same rule.
pub fn what_one_address_chunk_may_be() -> String {
    format!(
        "An address is <machine>/<stream id>/<node>/<port>, leading parts omitted meaning here, \
         and each part is one address chunk: a name already in the form every exposed name is \
         cast to — lowercase a-z 0-9 - . _ ~, no leading, trailing or doubled '-', at most \
         {EXPOSED_NAME_MAXIMUM_LENGTH} characters, and neither '.' nor '..'"
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

    /// Every character the cast keeps is legal in a chunk, a leading digit, `.`,
    /// `_` and `~` among them, so a chunk is never narrower than the cast.
    #[test]
    fn every_name_in_cast_form_is_one_chunk() {
        for legal in [
            "desk",
            "desk.tail2950db.ts.net",
            "rig-desk-a1b2",
            "camera-1_a.b",
            "desk.rig~1",
            "1st_pass",
            "_private",
            "v1.2",
            "~",
        ] {
            assert!(
                is_one_legal_address_chunk(legal),
                "{legal:?} must be one legal address chunk"
            );
        }
    }

    /// Every case of the shared cast fixture that the cast keeps is one chunk,
    /// and every name it would still change or refuses is not.
    #[test]
    fn a_chunk_is_exactly_what_the_cast_leaves_as_it_is() {
        #[derive(serde::Deserialize)]
        struct ExposedNameCastCase {
            name: String,
            #[serde(default)]
            cast: Option<String>,
        }
        let cases: Vec<ExposedNameCastCase> = serde_json::from_str(include_str!(
            "../../../tests/fixtures/exposed_name_cast_cases.json"
        ))
        .expect("the shared cast fixture parses");
        for case in cases {
            match case.cast {
                Some(cast) => {
                    assert!(is_one_legal_address_chunk(&cast), "{cast:?}");
                    assert_eq!(
                        is_one_legal_address_chunk(&case.name),
                        case.name == cast,
                        "{:?} is a chunk exactly when the cast leaves it as it is",
                        case.name
                    );
                }
                None => assert!(!is_one_legal_address_chunk(&case.name), "{:?}", case.name),
            }
        }
    }

    /// Each refused shape reads back with the reason the rule gives it.
    #[test]
    fn every_refused_shape_reads_back_with_its_reason() {
        assert_eq!(
            first_reason_this_is_not_one_address_chunk("").as_deref(),
            Some("it is empty")
        );
        for (candidate, its_cast) in [
            ("Camera", "camera"),
            ("desk/rig", "desk-rig"),
            ("slow sink", "slow-sink"),
            ("@runtime", "runtime"),
            ("a--b", "a-b"),
            ("trailing-", "trailing"),
        ] {
            let reason = first_reason_this_is_not_one_address_chunk(candidate)
                .unwrap_or_else(|| panic!("{candidate:?} is not one chunk"));
            assert!(reason.contains(&format!("`{its_cast}`")), "{reason}");
        }
        for names_nothing in [".", "..", "!!!"] {
            let reason = first_reason_this_is_not_one_address_chunk(names_nothing)
                .unwrap_or_else(|| panic!("{names_nothing:?} is not one chunk"));
            assert!(reason.contains("cannot name anything"), "{reason}");
        }
    }

    /// The rule a refusal ends with spells the four-part address.
    #[test]
    fn the_rule_spells_the_four_part_address() {
        assert!(what_one_address_chunk_may_be().contains("<machine>/<stream id>/<node>/<port>"));
    }
}
