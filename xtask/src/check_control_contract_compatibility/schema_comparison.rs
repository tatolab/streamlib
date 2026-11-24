// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The comparison of one schema in the earlier contract file with the schema in the same place
//! of the later one, from the side of the wire its values travel from.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::the_list_under;

/// Marks a set, a `oneOf`, an `anyOf` or an `enum`, that may gain a member in what a client
/// reads. A client takes its open alternative from the file it was built against, so the mark
/// counts only where the earlier file states it.
const SET_THAT_MAY_GROW_MARK: &str = "x-may-grow";

/// Which side of the wire a schema's values travel from.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SchemaDirection {
    /// A client sends it: params.
    SentByAClient,
    /// A client reads it: a result, an error's data, or a notification's params.
    ReadByAClient,
}

impl SchemaDirection {
    pub(super) fn as_words(self) -> &'static str {
        match self {
            Self::SentByAClient => "as sent",
            Self::ReadByAClient => "as read",
        }
    }
}

pub(super) struct ContractComparison<'contracts> {
    earlier_contract: &'contracts Value,
    later_contract: &'contracts Value,
    schema_pairs_compared: BTreeSet<(String, String, SchemaDirection)>,
    breaking_differences: Vec<String>,
}

impl<'contracts> ContractComparison<'contracts> {
    pub(super) fn new(
        earlier_contract: &'contracts Value,
        later_contract: &'contracts Value,
    ) -> Self {
        Self {
            earlier_contract,
            later_contract,
            schema_pairs_compared: BTreeSet::new(),
            breaking_differences: Vec::new(),
        }
    }

    /// Record one breaking difference.
    pub(super) fn refuse(&mut self, breaking_difference: String) {
        self.breaking_differences.push(breaking_difference);
    }

    pub(super) fn into_breaking_differences(self) -> Vec<String> {
        self.breaking_differences
    }

    pub(super) fn compare_schemas(
        &mut self,
        path: &str,
        earlier_schema: &'contracts Value,
        later_schema: &'contracts Value,
        direction: SchemaDirection,
    ) {
        if let (Some(earlier_reference), Some(later_reference)) = (
            earlier_schema["$ref"].as_str(),
            later_schema["$ref"].as_str(),
        ) {
            if self.schema_pairs_compared.insert((
                earlier_reference.to_owned(),
                later_reference.to_owned(),
                direction,
            )) {
                self.compare_the_keywords_of(
                    path,
                    the_schema_a_reference_names(self.earlier_contract, earlier_schema),
                    the_schema_a_reference_names(self.later_contract, later_schema),
                    direction,
                );
            }
            // The keywords beside each `$ref` narrow the component where it is used, so they are
            // compared at every use, however often the component itself was. Where only one side
            // holds a `$ref`, its component is compared with the other side and the keywords
            // beside it are not.
            self.compare_the_keywords_of(path, earlier_schema, later_schema, direction);
            return;
        }
        self.compare_the_keywords_of(
            path,
            the_schema_a_reference_names(self.earlier_contract, earlier_schema),
            the_schema_a_reference_names(self.later_contract, later_schema),
            direction,
        );
    }

    /// Compare the keywords `earlier_schema` and `later_schema` state themselves, leaving a
    /// `$ref` either holds to the caller.
    fn compare_the_keywords_of(
        &mut self,
        path: &str,
        earlier_schema: &'contracts Value,
        later_schema: &'contracts Value,
        direction: SchemaDirection,
    ) {
        let (Some(earlier_object), Some(later_object)) =
            (earlier_schema.as_object(), later_schema.as_object())
        else {
            return;
        };
        let may_grow = earlier_object.get(SET_THAT_MAY_GROW_MARK) == Some(&Value::Bool(true));

        let earlier_types = the_values_listed_under(earlier_object.get("type"));
        let later_types = the_values_listed_under(later_object.get("type"));
        if !earlier_types.is_empty() && !later_types.is_empty() {
            let (must_hold, held_in, what_changed) = match direction {
                SchemaDirection::SentByAClient => (&earlier_types, &later_types, "no longer takes"),
                SchemaDirection::ReadByAClient => (&later_types, &earlier_types, "may now be"),
            };
            for outside in must_hold.difference(held_in) {
                self.refuse(format!("{path} {what_changed} the type {outside}"));
            }
        }
        let earlier_values = the_values_listed_under(earlier_object.get("enum"));
        let later_values = the_values_listed_under(later_object.get("enum"));
        if !earlier_values.is_empty() && !later_values.is_empty() {
            for removed in earlier_values.difference(&later_values) {
                self.refuse_a_removed_member(path, &format!("the value {removed}"), direction);
            }
            for added in later_values.difference(&earlier_values) {
                self.refuse_an_added_member(
                    path,
                    &format!("the value {added}"),
                    direction,
                    may_grow,
                );
            }
        }
        if let (Some(earlier_const), Some(later_const)) =
            (earlier_object.get("const"), later_object.get("const"))
            && earlier_const != later_const
        {
            self.refuse(format!(
                "{path} changed from the value {earlier_const} to {later_const}"
            ));
        }

        let earlier_required = the_values_listed_under(earlier_object.get("required"));
        let later_required = the_values_listed_under(later_object.get("required"));
        match direction {
            SchemaDirection::SentByAClient => {
                for now_required in later_required.difference(&earlier_required) {
                    self.refuse(format!("{path} now requires the field {now_required}"));
                }
            }
            SchemaDirection::ReadByAClient => {
                for no_longer_required in earlier_required.difference(&later_required) {
                    self.refuse(format!(
                        "{path} no longer always carries the field {no_longer_required}"
                    ));
                }
            }
        }

        let later_properties = later_object.get("properties").and_then(Value::as_object);
        for (field, earlier_field_schema) in earlier_object
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match later_properties.and_then(|properties| properties.get(field)) {
                Some(later_field_schema) => self.compare_schemas(
                    &format!("{path}.{field}"),
                    earlier_field_schema,
                    later_field_schema,
                    direction,
                ),
                None => self.refuse(format!("{path} lost the field \"{field}\"")),
            }
        }

        for nested in ["items", "additionalProperties"] {
            if let (Some(earlier_nested), Some(later_nested)) =
                (earlier_object.get(nested), later_object.get(nested))
            {
                self.compare_schemas(
                    &format!("{path}.{nested}"),
                    earlier_nested,
                    later_nested,
                    direction,
                );
            }
        }
        for alternatives in ["oneOf", "anyOf"] {
            if let (Some(earlier_alternatives), Some(later_alternatives)) = (
                earlier_object.get(alternatives),
                later_object.get(alternatives),
            ) {
                self.compare_alternatives(
                    &format!("{path}.{alternatives}"),
                    the_list_under(earlier_alternatives),
                    the_list_under(later_alternatives),
                    direction,
                    may_grow,
                );
            }
        }
    }

    /// Alternatives that carry a tag are matched by it, wherever each sits; the rest are matched
    /// by shape first, then by a `type` only one of them states, then in order.
    fn compare_alternatives(
        &mut self,
        path: &str,
        earlier_alternatives: &'contracts [Value],
        later_alternatives: &'contracts [Value],
        direction: SchemaDirection,
        may_grow: bool,
    ) {
        let earlier = the_alternatives_by_tag(self.earlier_contract, earlier_alternatives);
        let later = the_alternatives_by_tag(self.later_contract, later_alternatives);

        for (tag, earlier_alternative) in &earlier.tagged {
            match later.tagged.get(tag) {
                Some(later_alternative) => self.compare_schemas(
                    &format!("{path}[{tag}]"),
                    earlier_alternative,
                    later_alternative,
                    direction,
                ),
                None => self.refuse_a_removed_member(path, tag, direction),
            }
        }
        for tag in later
            .tagged
            .keys()
            .filter(|tag| !earlier.tagged.contains_key(*tag))
        {
            self.refuse_an_added_member(path, tag, direction, may_grow);
        }

        let untagged = UntaggedPairing::of(
            self.earlier_contract,
            &earlier.untagged,
            self.later_contract,
            &later.untagged,
        );
        for (earlier_index, later_index) in untagged.pairs {
            self.compare_schemas(
                &format!("{path}[{later_index}]"),
                earlier.untagged[earlier_index],
                later.untagged[later_index],
                direction,
            );
        }
        // An untagged alternative names no member a client branches on, so only what a client
        // sends feels one leave.
        if direction == SchemaDirection::SentByAClient {
            for earlier_index in untagged.earlier_left_over {
                let words = the_words_for(earlier.untagged[earlier_index], earlier_index);
                self.refuse_a_removed_member(path, &words, direction);
            }
        }
        for later_index in untagged.later_left_over {
            let words = the_words_for(later.untagged[later_index], later_index);
            self.refuse_an_added_member(path, &words, direction, may_grow);
        }
    }

    fn refuse_a_removed_member(&mut self, path: &str, member: &str, direction: SchemaDirection) {
        self.refuse(match direction {
            SchemaDirection::SentByAClient => format!("{path} no longer takes {member}"),
            SchemaDirection::ReadByAClient => format!("{path} can no longer be {member}"),
        });
    }

    /// A member added to what a client sends is an addition; to what it reads, a break unless
    /// the earlier file marked the set to grow.
    fn refuse_an_added_member(
        &mut self,
        path: &str,
        member: &str,
        direction: SchemaDirection,
        may_grow: bool,
    ) {
        if direction == SchemaDirection::ReadByAClient && !may_grow {
            self.refuse(format!(
                "{path} may now be {member}, and the set is not marked `{SET_THAT_MAY_GROW_MARK}`"
            ));
        }
    }
}

struct AlternativesByTag<'contracts> {
    /// Each alternative a tag names, by the tag in words.
    tagged: BTreeMap<String, &'contracts Value>,
    /// The others, in order.
    untagged: Vec<&'contracts Value>,
}

/// `alternatives` split by whether a tag tells each apart; a tag a second alternative repeats
/// leaves that one untagged.
fn the_alternatives_by_tag<'contracts>(
    contract: &'contracts Value,
    alternatives: &'contracts [Value],
) -> AlternativesByTag<'contracts> {
    let mut by_tag = AlternativesByTag {
        tagged: BTreeMap::new(),
        untagged: Vec::new(),
    };
    for alternative in alternatives {
        match the_tag_of(the_schema_a_reference_names(contract, alternative)) {
            Some(tag) if !by_tag.tagged.contains_key(&tag) => {
                by_tag.tagged.insert(tag, alternative);
            }
            _ => by_tag.untagged.push(alternative),
        }
    }
    by_tag
}

/// Which earlier untagged alternative each later one stands for, by index into each list.
struct UntaggedPairing {
    pairs: Vec<(usize, usize)>,
    earlier_left_over: Vec<usize>,
    later_left_over: Vec<usize>,
}

impl UntaggedPairing {
    fn of(
        earlier_contract: &Value,
        earlier: &[&Value],
        later_contract: &Value,
        later: &[&Value],
    ) -> Self {
        let mut pairing = Self {
            pairs: Vec::new(),
            earlier_left_over: (0..earlier.len()).collect(),
            later_left_over: (0..later.len()).collect(),
        };
        pairing.pair_where(|earlier_index, later_index, _| {
            earlier[earlier_index] == later[later_index]
        });
        let the_types_of = |contract: &Value, alternatives: &[&Value]| -> Vec<Option<String>> {
            alternatives
                .iter()
                .map(|alternative| {
                    the_schema_a_reference_names(contract, alternative)
                        .get("type")
                        .map(Value::to_string)
                })
                .collect()
        };
        let earlier_types = the_types_of(earlier_contract, earlier);
        let later_types = the_types_of(later_contract, later);
        let how_many_left_over_state = |left_over: &[usize], types: &[Option<String>], stated| {
            left_over
                .iter()
                .filter(|index| types[**index].as_ref() == Some(stated))
                .count()
        };
        pairing.pair_where(|earlier_index, later_index, pairing| {
            let Some(stated) = earlier_types[earlier_index].as_ref() else {
                return false;
            };
            later_types[later_index].as_ref() == Some(stated)
                && how_many_left_over_state(&pairing.earlier_left_over, &earlier_types, stated) == 1
                && how_many_left_over_state(&pairing.later_left_over, &later_types, stated) == 1
        });
        pairing.pair_where(|_, _, _| true);
        pairing
    }

    /// Pair each left-over earlier alternative, in order, with the first left-over later one
    /// `matches` accepts.
    fn pair_where(&mut self, matches: impl Fn(usize, usize, &Self) -> bool) {
        let mut earlier_position = 0;
        while earlier_position < self.earlier_left_over.len() {
            let earlier_index = self.earlier_left_over[earlier_position];
            match self
                .later_left_over
                .iter()
                .position(|later_index| matches(earlier_index, *later_index, self))
            {
                Some(later_position) => {
                    let later_index = self.later_left_over.remove(later_position);
                    self.earlier_left_over.remove(earlier_position);
                    self.pairs.push((earlier_index, later_index));
                }
                None => earlier_position += 1,
            }
        }
    }
}

/// An untagged alternative in words: the component it names, the type it states, or its place.
fn the_words_for(alternative: &Value, index: usize) -> String {
    if let Some(reference) = alternative["$ref"].as_str() {
        return format!("the alternative {reference}");
    }
    match alternative.get("type") {
        Some(stated) => format!("the alternative of type {stated}"),
        None => format!("the alternative [{index}]"),
    }
}

/// What tells `alternative` apart from the others of its set, in words: its `const`, the
/// `const` of a field it requires, or the one field it requires and holds, as an externally
/// tagged variant has.
fn the_tag_of(alternative: &Value) -> Option<String> {
    if let Some(value) = alternative.get("const") {
        return Some(format!("the value {value}"));
    }
    let properties = alternative.get("properties").and_then(Value::as_object)?;
    let required: Vec<&str> = the_list_under(&alternative["required"])
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if let Some((field, value)) = required.iter().find_map(|field| {
        properties
            .get(*field)
            .and_then(|schema| schema.get("const"))
            .map(|value| (field, value))
    }) {
        return Some(format!("the alternative whose \"{field}\" is {value}"));
    }
    match (required.as_slice(), properties.len()) {
        ([field], 1) if properties.contains_key(*field) => {
            Some(format!("the alternative keyed \"{field}\""))
        }
        _ => None,
    }
}

/// The schema `schema` is, following a `$ref` into its contract's `components.schemas`.
fn the_schema_a_reference_names<'contract>(
    contract: &'contract Value,
    schema: &'contract Value,
) -> &'contract Value {
    match schema["$ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
    {
        Some(schema_name) => &contract["components"]["schemas"][schema_name],
        None => schema,
    }
}

/// The values a keyword lists, a lone value read as a list of one, each as its JSON text.
fn the_values_listed_under(keyword: Option<&Value>) -> BTreeSet<String> {
    match keyword {
        Some(Value::Array(values)) => values.iter().map(Value::to_string).collect(),
        Some(value) => BTreeSet::from([value.to_string()]),
        None => BTreeSet::new(),
    }
}
