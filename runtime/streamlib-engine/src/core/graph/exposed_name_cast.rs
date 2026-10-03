// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The cast every exposed name — machine, stream, node, port — goes through.
//!
//! The wheel's pure-Python twin (`streamlib._exposed_name_cast`) casts the same
//! way, and both are held to `tests/fixtures/exposed_name_cast_cases.json`.

use std::borrow::Cow;

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

use crate::core::error::{Error, Result};

/// Longest name the cast keeps, in characters — a DNS label's bound.
pub const EXPOSED_NAME_MAXIMUM_LENGTH: usize = 63;

const EXPOSED_NAME_REPLACEMENT_CHARACTER: char = '-';

/// Cast `name` to lowercase RFC 3986 unreserved characters (`a-z 0-9 - . _ ~`).
///
/// Accents are dropped, every other character becomes `-`, runs of `-` collapse,
/// `-` is trimmed from both ends and the result is cut to
/// [`EXPOSED_NAME_MAXIMUM_LENGTH`]. A name casting to empty, `.` or `..` is
/// [`Error::ExposedNameCastsToNothing`]. A name already in cast form comes back
/// borrowed, so a lookup on the per-bag path allocates nothing.
pub fn cast_exposed_name_to_url_safe(name: &str) -> Result<Cow<'_, str>> {
    if is_already_cast(name) {
        return Ok(Cow::Borrowed(name));
    }
    let mut cast = String::with_capacity(name.len());
    let lowercased_without_accents = name
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase);
    for character in lowercased_without_accents {
        let kept = if is_rfc_3986_unreserved(character) {
            character
        } else {
            EXPOSED_NAME_REPLACEMENT_CHARACTER
        };
        if kept == EXPOSED_NAME_REPLACEMENT_CHARACTER
            && cast.ends_with(EXPOSED_NAME_REPLACEMENT_CHARACTER)
        {
            continue;
        }
        cast.push(kept);
    }

    let trimmed: String = cast
        .trim_matches(EXPOSED_NAME_REPLACEMENT_CHARACTER)
        .chars()
        .take(EXPOSED_NAME_MAXIMUM_LENGTH)
        .collect();
    let trimmed = trimmed.trim_end_matches(EXPOSED_NAME_REPLACEMENT_CHARACTER);

    if matches!(trimmed, "" | "." | "..") {
        return Err(Error::ExposedNameCastsToNothing {
            name: name.to_string(),
            cast: trimmed.to_string(),
        });
    }
    Ok(Cow::Owned(trimmed.to_string()))
}

fn is_already_cast(name: &str) -> bool {
    name.len() <= EXPOSED_NAME_MAXIMUM_LENGTH
        && !matches!(name, "" | "." | "..")
        && name.chars().all(is_rfc_3986_unreserved)
        && !name.starts_with(EXPOSED_NAME_REPLACEMENT_CHARACTER)
        && !name.ends_with(EXPOSED_NAME_REPLACEMENT_CHARACTER)
        && !name.contains("--")
}

fn is_rfc_3986_unreserved(character: char) -> bool {
    character.is_ascii_lowercase()
        || character.is_ascii_digit()
        || matches!(character, '-' | '.' | '_' | '~')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct ExposedNameCastCase {
        name: String,
        #[serde(default)]
        cast: Option<String>,
        #[serde(default)]
        refused: bool,
    }

    #[test]
    fn every_shared_fixture_case_casts_as_written() {
        let cases: Vec<ExposedNameCastCase> = serde_json::from_str(include_str!(
            "../../../tests/fixtures/exposed_name_cast_cases.json"
        ))
        .expect("the shared cast fixture parses");
        assert!(!cases.is_empty());
        for case in cases {
            let outcome = cast_exposed_name_to_url_safe(&case.name);
            if case.refused {
                assert!(
                    matches!(outcome, Err(Error::ExposedNameCastsToNothing { .. })),
                    "{:?} should be refused, got {outcome:?}",
                    case.name
                );
            } else {
                assert_eq!(
                    outcome.ok().map(Cow::into_owned),
                    case.cast,
                    "{:?} cast differently from the fixture",
                    case.name
                );
            }
        }
    }

    #[test]
    fn a_cast_name_casts_to_itself() {
        for name in ["camerasource", "front-camera", "v1.2~beta", "video_from_upstream"] {
            assert!(matches!(
                cast_exposed_name_to_url_safe(name).unwrap(),
                Cow::Borrowed(borrowed) if borrowed == name
            ));
        }
    }
}
