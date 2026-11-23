// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The members of a bundle as the contract's `PinType` schema states them, so a client checking
//! a pin type by the schema refuses each bundle [`PinType::validate`] refuses: a member that is
//! neither a small type, a List of small types nor a heavy type, unless every member is a
//! Tensor, and a member named twice.

use schemars_1::Schema;
use serde_json::{Map, Value, json};

use super::BuiltInPinTypeName;
#[cfg(doc)]
use super::PinType;

/// The keyword the client's checker reads on a list: no two of its items hold the same value
/// under the key it names.
pub const ITEMS_UNIQUE_BY_KEYWORD: &str = "x-unique-by";

/// The key whose value names a bundle member, which no two members of one bundle share.
const BUNDLE_MEMBER_NAME_KEY: &str = "name";

/// `schema`, the generated schema of [`PinType`], with the `members` of its bundle alternative
/// stating what a member may be and that each has a name of its own. A schema of another shape
/// is left as it is.
pub(super) fn state_what_a_bundle_member_may_be(schema: &mut Schema) {
    let Some(alternatives) = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .map(|alternatives| alternatives.to_vec())
    else {
        return;
    };
    let alternatives_named = |taken: fn(BuiltInPinTypeName) -> bool| -> Vec<Value> {
        alternatives
            .iter()
            .filter(|alternative| the_name_of(alternative).is_some_and(taken))
            .cloned()
            .collect()
    };
    let small_types = alternatives_named(BuiltInPinTypeName::is_a_small_type);
    let heavy_types = alternatives_named(BuiltInPinTypeName::is_a_heavy_type);
    let tensors = alternatives_named(|name| name == BuiltInPinTypeName::Tensor);
    let Some(mut list_of_small_types) =
        alternatives_named(|name| name == BuiltInPinTypeName::ListOf)
            .into_iter()
            .next()
    else {
        return;
    };
    list_of_small_types["properties"]["element_type"] = json!({ "oneOf": small_types });
    let mut a_member_of_a_mixed_bundle = small_types;
    a_member_of_a_mixed_bundle.extend(heavy_types);
    a_member_of_a_mixed_bundle.push(list_of_small_types);

    let Some(oneof) = schema.get_mut("oneOf").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(members) = oneof
        .iter_mut()
        .find(|alternative| the_name_of(alternative) == Some(BuiltInPinTypeName::Bundle))
        .and_then(|bundle| bundle.get_mut("properties"))
        .and_then(|properties| properties.get_mut("members"))
    else {
        return;
    };
    let Some(member_reference) = members.pointer("/items/$ref").cloned() else {
        return;
    };
    let a_list_of_members_typed = |member_types: Vec<Value>| {
        json!({
            "items": {
                "$ref": member_reference,
                "properties": { "member_type": { "oneOf": member_types } },
            },
        })
    };
    let mut stated = Map::new();
    stated.insert("type".to_owned(), json!("array"));
    stated.insert("items".to_owned(), json!({ "$ref": member_reference }));
    if let Some(description) = members.get("description") {
        stated.insert("description".to_owned(), description.clone());
    }
    stated.insert(
        ITEMS_UNIQUE_BY_KEYWORD.to_owned(),
        json!(BUNDLE_MEMBER_NAME_KEY),
    );
    stated.insert(
        "anyOf".to_owned(),
        json!([
            a_list_of_members_typed(a_member_of_a_mixed_bundle),
            a_list_of_members_typed(tensors),
        ]),
    );
    *members = Value::Object(stated);
}

/// The built-in type an alternative of the [`PinType`] schema states by its `type` tag.
fn the_name_of(alternative: &Value) -> Option<BuiltInPinTypeName> {
    let tag = alternative.pointer("/properties/type/const")?;
    serde_json::from_value(tag.clone()).ok()
}
