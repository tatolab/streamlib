// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `@tatolab/runtime-client`'s `control_contract_schemas.js`: every schema of the control
//! contract as data, its descriptions dropped, which the hand-written `control_contract_check.js`
//! checks each request, answer and notification against. A schema keyword the checker does not
//! implement stops generation, so no construct reaches the client unchecked.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use serde_json::{Map, Value};

use crate::contract_component_references::COMPONENT_SCHEMA_REFERENCE_PREFIX;
use crate::generate_runtime_client::{
    ControlContractEntryRead, GENERATED_SOURCE_HEADER, READS_ITS_DEFAULT_MARK,
    SET_THAT_MAY_GROW_MARK, property_key,
};

/// The keyword stating that no two items of a list hold the same value under the key it names,
/// as the members of a bundle each hold a name of their own.
const ITEMS_UNIQUE_BY_KEYWORD: &str = "x-unique-by";

/// The keywords `control_contract_check.js` checks a value against whose stated value, once its
/// shape is right, needs no further look here.
const KEYWORDS_CHECKED_AS_STATED: &[&str] = &[
    ITEMS_UNIQUE_BY_KEYWORD,
    "const",
    "minimum",
    "exclusiveMinimum",
    "minItems",
    "maxItems",
    "required",
];

/// The keywords that state no check: the checker reads past them. `x-removed-in` names the
/// contract version a field leaves at, which a value of this version still carries.
const KEYWORDS_THAT_STATE_NO_CHECK: &[&str] = &["description", "default", "x-removed-in"];

/// The JSON types a `type` may name.
const JSON_TYPE_NAMES: &[&str] = &[
    "string", "integer", "number", "boolean", "null", "array", "object",
];

/// The formats the checker knows: `uuid` is checked as one, an integer format by its range, and a
/// floating-point one states no check.
const FORMATS_THE_CHECKER_KNOWS: &[&str] = &[
    "uuid", "double", "float", "int8", "int16", "int32", "int64", "uint8", "uint16", "uint32",
    "uint64", "uint",
];

/// Refuses the first schema of the contract that states a keyword, type or format the checker
/// does not implement, naming where it is.
pub(crate) fn refuse_a_schema_the_checker_does_not_know(
    component_schemas: &Map<String, Value>,
    methods: &[ControlContractEntryRead<'_>],
    notifications: &[ControlContractEntryRead<'_>],
) -> Result<()> {
    for (name, schema) in component_schemas {
        refuse_what_the_checker_does_not_know(
            schema,
            &format!("components.schemas.{name}"),
            component_schemas,
        )?;
    }
    for method in methods {
        refuse_what_the_checker_does_not_know(
            method.params_schema,
            &format!("the params of `{}`", method.name),
            component_schemas,
        )?;
        if let Some(result_schema) = method.result_schema {
            refuse_what_the_checker_does_not_know(
                result_schema,
                &format!("the result of `{}`", method.name),
                component_schemas,
            )?;
        }
    }
    for notification in notifications {
        refuse_what_the_checker_does_not_know(
            notification.params_schema,
            &format!("the params of the notification `{}`", notification.name),
            component_schemas,
        )?;
    }
    Ok(())
}

fn refuse_what_the_checker_does_not_know(
    schema: &Value,
    place: &str,
    component_schemas: &Map<String, Value>,
) -> Result<()> {
    let keywords = match schema {
        Value::Bool(_) => return Ok(()),
        Value::Object(keywords) => keywords,
        _ => bail!("{place} is neither a schema object nor `true` or `false`"),
    };
    for (keyword, stated) in keywords {
        let place_of_the_keyword = format!("{place}.{keyword}");
        match keyword.as_str() {
            "properties" => {
                let Some(properties) = stated.as_object() else {
                    bail!("{place_of_the_keyword} is not an object of schemas");
                };
                for (property_name, property_schema) in properties {
                    refuse_what_the_checker_does_not_know(
                        property_schema,
                        &format!("{place_of_the_keyword}.{property_name}"),
                        component_schemas,
                    )?;
                }
            }
            "items" | "additionalProperties" => {
                refuse_what_the_checker_does_not_know(
                    stated,
                    &place_of_the_keyword,
                    component_schemas,
                )?;
            }
            "oneOf" | "anyOf" => {
                let Some(alternatives) = stated.as_array().filter(|list| !list.is_empty()) else {
                    bail!("{place_of_the_keyword} is not a list of alternatives");
                };
                for (position, alternative) in alternatives.iter().enumerate() {
                    refuse_what_the_checker_does_not_know(
                        alternative,
                        &format!("{place_of_the_keyword}.{position}"),
                        component_schemas,
                    )?;
                }
            }
            "$ref" => {
                let named = stated.as_str().and_then(|reference| {
                    reference.strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
                });
                if !named.is_some_and(|name| component_schemas.contains_key(name)) {
                    bail!(
                        "{place_of_the_keyword} names {stated}, which is no component of the contract"
                    );
                }
            }
            "type" => {
                let type_names = match stated {
                    Value::Array(type_names) => type_names.as_slice(),
                    one => std::slice::from_ref(one),
                };
                for type_name in type_names {
                    if !type_name
                        .as_str()
                        .is_some_and(|type_name| JSON_TYPE_NAMES.contains(&type_name))
                    {
                        bail!(
                            "{place_of_the_keyword} names the type {type_name}, which the client's checker does not know"
                        );
                    }
                }
            }
            "format" => {
                if !stated
                    .as_str()
                    .is_some_and(|format| FORMATS_THE_CHECKER_KNOWS.contains(&format))
                {
                    bail!(
                        "{place_of_the_keyword} names the format {stated}, which the client's checker does not know"
                    );
                }
            }
            "minimum" | "exclusiveMinimum" if !stated.is_number() => {
                bail!("{place_of_the_keyword} is not a number")
            }
            "minItems" | "maxItems" if !stated.is_u64() => {
                bail!("{place_of_the_keyword} is not a whole number")
            }
            ITEMS_UNIQUE_BY_KEYWORD if !stated.is_string() => {
                bail!("{place_of_the_keyword} names no key the items are unique by")
            }
            "required"
                if !stated
                    .as_array()
                    .is_some_and(|names| names.iter().all(Value::is_string)) =>
            {
                bail!("{place_of_the_keyword} is not a list of property names")
            }
            SET_THAT_MAY_GROW_MARK => {
                let members = keywords.get("oneOf").and_then(Value::as_array);
                let its_last_member_is_a_constant = members
                    .and_then(|members| members.last())
                    .is_some_and(|last| last.get("const").is_some());
                let each_member_is_an_object_its_tag_names = members.is_some_and(|members| {
                    members.iter().all(|member| {
                        the_tag_of_an_object_member(member, component_schemas).is_some()
                    })
                });
                if *stated != Value::Bool(true)
                    || !(its_last_member_is_a_constant || each_member_is_an_object_its_tag_names)
                {
                    bail!(
                        "{place_of_the_keyword} marks no `oneOf` of constants whose last a member \
                         it does not name could read as, nor one of objects each named by the \
                         constant of one property, whose unknown tag a client reads as it is"
                    );
                }
            }
            keyword
                if KEYWORDS_CHECKED_AS_STATED.contains(&keyword)
                    || KEYWORDS_THAT_STATE_NO_CHECK.contains(&keyword)
                    || keyword == READS_ITS_DEFAULT_MARK => {}
            keyword => bail!(
                "{place} states `{keyword}`, a keyword the client's checker \
                 (sdk/tatolab-runtime-client/control_contract_check.js) does not implement: \
                 implement it there and name it in the generator, or state the shape another way"
            ),
        }
    }
    Ok(())
}

/// The property a member of a `oneOf` is told apart by, with its constant: the one property its
/// object, or the component its `$ref` names, states a `const` for.
fn the_tag_of_an_object_member<'schema>(
    member: &'schema Value,
    component_schemas: &'schema Map<String, Value>,
) -> Option<(&'schema str, &'schema Value)> {
    let member = match member
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX))
    {
        Some(name) => component_schemas.get(name)?,
        None => member,
    };
    if member.get("type") != Some(&Value::String("object".to_owned())) {
        return None;
    }
    let mut tags = member
        .get("properties")?
        .as_object()?
        .iter()
        .filter_map(|(name, property)| property.get("const").map(|tag| (name.as_str(), tag)));
    let tag = tags.next()?;
    tags.next().is_none().then_some(tag)
}

/// `schema` as the checker reads it: no `description`, and a `default` only where the client
/// reads it into an absent property.
fn schema_as_the_checker_reads_it(schema: &Value) -> Value {
    let Value::Object(keywords) = schema else {
        return schema.clone();
    };
    let reads_its_default = keywords.get(READS_ITS_DEFAULT_MARK) == Some(&Value::Bool(true));
    let mut read = Map::new();
    for (keyword, stated) in keywords {
        let kept = match keyword.as_str() {
            "description" => continue,
            "default" if !reads_its_default => continue,
            "properties" => Value::Object(
                stated
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, property)| {
                        (name.clone(), schema_as_the_checker_reads_it(property))
                    })
                    .collect(),
            ),
            "oneOf" | "anyOf" => Value::Array(
                stated
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(schema_as_the_checker_reads_it)
                    .collect(),
            ),
            "items" | "additionalProperties" => schema_as_the_checker_reads_it(stated),
            _ => stated.clone(),
        };
        read.insert(keyword.clone(), kept);
    }
    Value::Object(read)
}

fn compact_json_of(schema: &Value) -> String {
    serde_json::to_string(&schema_as_the_checker_reads_it(schema))
        .unwrap_or_else(|_| unreachable!("a JSON value always serializes"))
}

/// `control_contract_schemas.js`: one line per component, per method's params and result, and
/// per notification's params, frozen throughout.
pub(crate) fn contract_schemas_javascript(
    component_schemas: &Map<String, Value>,
    methods: &[ControlContractEntryRead<'_>],
    notifications: &[ControlContractEntryRead<'_>],
) -> String {
    let mut schemas = String::from(GENERATED_SOURCE_HEADER);
    schemas.push_str(
        "\n// @ts-check\n\n\
         /** @import { ControlContractSchemas } from \"./control_contract_schemas.js\" */\n\n\
         /**\n * @template Value\n * @param {Value} value\n * @returns {Value}\n */\n\
         function frozenThroughout(value) {\n  \
         if (typeof value === \"object\" && value !== null) {\n    \
         for (const member of Object.values(value)) frozenThroughout(member);\n    \
         Object.freeze(value);\n  }\n  return value;\n}\n\n\
         export const CONTROL_CONTRACT_SCHEMAS = frozenThroughout(\n  /** @satisfies {ControlContractSchemas} */ ({\n  components: {\n",
    );
    for (name, schema) in component_schemas {
        let _ = writeln!(
            schemas,
            "    {}: {},",
            property_key(name),
            compact_json_of(schema)
        );
    }
    schemas.push_str("  },\n  operations: {\n");
    for method in methods {
        let _ = writeln!(
            schemas,
            "    {}: {{ params: {}, result: {} }},",
            property_key(method.name),
            compact_json_of(method.params_schema),
            method
                .result_schema
                .map_or_else(|| "true".to_owned(), compact_json_of),
        );
    }
    schemas.push_str("  },\n  notifications: {\n");
    for notification in notifications {
        let _ = writeln!(
            schemas,
            "    {}: {},",
            property_key(notification.name),
            compact_json_of(notification.params_schema),
        );
    }
    schemas.push_str("  },\n  }),\n);\n");
    schemas
}

/// `control_contract_schemas.d.ts`.
pub(crate) fn contract_schemas_declarations() -> String {
    let mut declarations = String::from(GENERATED_SOURCE_HEADER);
    declarations.push_str(
        "\nimport type {\n  ControlContractComponentName,\n  ControlNotificationMethodName,\n  ControlOperationMethodName,\n} from \"./control_contract.js\";\n\n\
         /** One JSON Schema of the contract as the client's checker reads it, its descriptions dropped. */\n\
         export type ControlContractSchema = boolean | { readonly [keyword: string]: unknown };\n\n\
         /** Every schema of the contract: by component, by method's params and result, and by notification's params. */\n\
         export interface ControlContractSchemas {\n  \
         readonly components: { readonly [Name in ControlContractComponentName]: ControlContractSchema };\n  \
         readonly operations: {\n    readonly [Method in ControlOperationMethodName]: { readonly params: ControlContractSchema; readonly result: ControlContractSchema };\n  };\n  \
         readonly notifications: { readonly [Method in ControlNotificationMethodName]: ControlContractSchema };\n}\n\n\
         /** Every schema of the contract, which the client checks each request, answer and notification against. */\n\
         export declare const CONTROL_CONTRACT_SCHEMAS: ControlContractSchemas;\n",
    );
    declarations
}

#[cfg(test)]
mod tests;
