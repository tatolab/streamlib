// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How a manifest a module carries reads its fields: the view manifest (`tatolab_view.json`) and
//! the node UI manifest (`node_ui/tatolab_node_ui.json`) both name a contract version, paths
//! inside their own folder and optional fields, and word a refusal by the field it names.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

/// The field a refusal names: a missing field by its own path, a field the manifest does not have
/// by the object holding it, the whole manifest by `manifest_file_name`.
pub(crate) fn the_field_a_serde_refusal_names(
    not_the_shape: &serde_path_to_error::Error<serde_json::Error>,
    manifest_file_name: &str,
) -> String {
    let why = not_the_shape.inner().to_string();
    let mut steps: Vec<String> = not_the_shape
        .path()
        .iter()
        .filter_map(|segment| match segment {
            serde_path_to_error::Segment::Map { key } => Some(format!(".{key}")),
            serde_path_to_error::Segment::Seq { index } => Some(format!("[{index}]")),
            _ => None,
        })
        .collect();
    if why.starts_with("unknown field ") {
        steps.pop();
    }
    if let Some((missing_field, _)) = why
        .strip_prefix("missing field `")
        .and_then(|after_the_quote| after_the_quote.split_once('`'))
    {
        steps.push(format!(".{missing_field}"));
    }
    let field_path = steps.concat();
    match field_path.strip_prefix('.') {
        Some(field_path) => field_path.to_owned(),
        None if field_path.is_empty() => manifest_file_name.to_owned(),
        None => field_path,
    }
}

/// An optional field present in the manifest, which is refused as `null`.
pub(crate) fn a_value_never_written_as_null<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| D::Error::custom("it is left out, never written as null"))
}

/// Whether `contract` is written `major.minor`, each a canonical unsigned integer.
pub(crate) fn is_a_contract_version(contract: &str) -> bool {
    contract.split_once('.').is_some_and(|(major, minor)| {
        is_a_canonical_unsigned_integer(major) && is_a_canonical_unsigned_integer(minor)
    })
}

/// Whether `contract` names `read_major` and a minor no higher than `highest_minor`.
pub(crate) fn is_a_contract_version_of_major_up_to_minor(
    contract: &str,
    read_major: u64,
    highest_minor: u64,
) -> bool {
    let Some((major, minor)) = contract.split_once('.') else {
        return false;
    };
    let (Ok(major), Ok(minor)) = (major.parse::<u64>(), minor.parse::<u64>()) else {
        return false;
    };
    major == read_major && minor <= highest_minor
}

/// A path inside the manifest's own folder: relative, with no `..` step, no scheme and no
/// backslash.
pub fn is_a_path_inside_its_own_folder(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || starts_with_a_scheme(path)
    {
        return false;
    }
    path.split('/')
        .all(|step| !step.is_empty() && step != "." && step != "..")
}

fn is_a_canonical_unsigned_integer(text: &str) -> bool {
    text == "0"
        || (!text.is_empty()
            && !text.starts_with('0')
            && text.bytes().all(|byte| byte.is_ascii_digit()))
}

fn starts_with_a_scheme(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-')
        })
}

#[cfg(test)]
mod tests;
