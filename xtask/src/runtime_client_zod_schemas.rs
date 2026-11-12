// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The zod source of `@tatolab/runtime-client`'s `control_schemas.js`: one zod schema per
//! component schema of the control contract file, and one per method's params and result and
//! per notification's params, read from the same JSON Schemas `control_contract.d.ts` is written
//! from.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::generate_runtime_client::{
    COMPONENT_SCHEMA_REFERENCE_PREFIX, GENERATED_SOURCE_HEADER, is_a_wide_integer_format,
    property_key,
};

/// One method or notification whose schemas `ControlSchemas` holds.
pub struct ZodSchemasOfOneContractEntry<'contract> {
    pub name: &'contract str,
    pub params_schema: &'contract Value,
    pub result_schema: Option<&'contract Value>,
}

/// `control_schemas.js`: the component schemas in an order where each comes after the ones it
/// names, then `ControlSchemas`, every schema by component, method and notification.
pub fn zod_schemas_javascript(
    component_schemas: &Map<String, Value>,
    methods: &[ZodSchemasOfOneContractEntry<'_>],
    notifications: &[ZodSchemasOfOneContractEntry<'_>],
) -> String {
    let declaration_order = component_names_each_after_the_ones_it_names(component_schemas);
    let declaration_position_by_name: BTreeMap<&str, usize> = declaration_order
        .iter()
        .enumerate()
        .map(|(position, name)| (*name, position))
        .collect();
    let mut schemas = String::from(GENERATED_SOURCE_HEADER);
    schemas.push_str(
        "\n// @ts-check\n\n\
         /** @import * as ControlContract from \"./control_contract.js\" */\n\
         /** @import { ControlSchemasOfTheContract } from \"./control_schemas.js\" */\n\n\
         import { z } from \"zod\";\n",
    );
    for (position, name) in declaration_order.iter().enumerate() {
        let zod_source = ZodSourceWriter {
            declaration_position_by_name: &declaration_position_by_name,
            position_being_declared: Some(position),
        };
        let _ = write!(
            schemas,
            "\n/** @type {{z.ZodType<ControlContract.{name}>}} */\nconst {name} = {};\n",
            zod_source.expression_of(&component_schemas[*name], 0)
        );
    }
    let zod_source = ZodSourceWriter {
        declaration_position_by_name: &declaration_position_by_name,
        position_being_declared: None,
    };
    schemas.push_str(
        "\n/** @satisfies {ControlSchemasOfTheContract} */\n\
         export const ControlSchemas = Object.freeze({\n  components: Object.freeze({\n",
    );
    for name in component_schemas.keys() {
        let _ = writeln!(schemas, "    {name},");
    }
    schemas.push_str("  }),\n  operations: Object.freeze({\n");
    for method in methods {
        let _ = writeln!(
            schemas,
            "    {}: Object.freeze({{\n      params: {},\n      result: {},\n    }}),",
            property_key(method.name),
            zod_source.expression_of(method.params_schema, 3),
            method.result_schema.map_or_else(
                || "z.unknown()".to_owned(),
                |schema| zod_source.expression_of(schema, 3)
            ),
        );
    }
    schemas.push_str("  }),\n  notifications: Object.freeze({\n");
    for notification in notifications {
        let _ = writeln!(
            schemas,
            "    {}: {},",
            property_key(notification.name),
            zod_source.expression_of(notification.params_schema, 2),
        );
    }
    schemas.push_str("  }),\n});\n");
    schemas
}

/// The component names, each after every component it names, except where two name each
/// other; a component named before it is declared is read through `z.lazy`.
fn component_names_each_after_the_ones_it_names(
    component_schemas: &Map<String, Value>,
) -> Vec<&str> {
    let mut declaration_order: Vec<&str> = Vec::with_capacity(component_schemas.len());
    let mut visited: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    fn visit<'contract>(
        name: &'contract str,
        component_schemas: &'contract Map<String, Value>,
        visited: &mut std::collections::BTreeSet<&'contract str>,
        declaration_order: &mut Vec<&'contract str>,
    ) {
        if !visited.insert(name) {
            return;
        }
        if let Some(schema) = component_schemas.get(name) {
            let mut named_components = Vec::new();
            component_names_a_schema_names(schema, &mut named_components);
            for named_component in named_components {
                if let Some((key, _)) = component_schemas.get_key_value(named_component) {
                    visit(key, component_schemas, visited, declaration_order);
                }
            }
            if let Some((key, _)) = component_schemas.get_key_value(name) {
                declaration_order.push(key);
            }
        }
    }
    for name in component_schemas.keys() {
        visit(
            name,
            component_schemas,
            &mut visited,
            &mut declaration_order,
        );
    }
    declaration_order
}

fn component_names_a_schema_names<'schema>(schema: &'schema Value, named: &mut Vec<&'schema str>) {
    match schema {
        Value::Object(members) => {
            if let Some(reference) = members.get("$ref").and_then(Value::as_str)
                && let Some(name) = reference.strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
            {
                named.push(name);
            }
            for member in members.values() {
                component_names_a_schema_names(member, named);
            }
        }
        Value::Array(items) => {
            for item in items {
                component_names_a_schema_names(item, named);
            }
        }
        _ => {}
    }
}

/// Writes the zod expression of a JSON Schema, naming a component it refers to by its
/// declared constant.
struct ZodSourceWriter<'order> {
    declaration_position_by_name: &'order BTreeMap<&'order str, usize>,
    /// The position of the component being declared, when the expression is one; a reference
    /// to a component at or after it is read lazily.
    position_being_declared: Option<usize>,
}

impl ZodSourceWriter<'_> {
    fn expression_of(&self, schema: &Value, indent_level: usize) -> String {
        match schema {
            Value::Bool(true) => return "z.unknown()".to_owned(),
            Value::Bool(false) => return "z.never()".to_owned(),
            _ => {}
        }
        if let Some(reference) = schema["$ref"].as_str() {
            let name = reference
                .strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
                .unwrap_or(reference);
            let declared_before = match (
                self.position_being_declared,
                self.declaration_position_by_name.get(name),
            ) {
                (None, _) => true,
                (Some(being_declared), Some(named)) => *named < being_declared,
                (Some(_), None) => false,
            };
            return if declared_before {
                name.to_owned()
            } else {
                format!("z.lazy(() => {name})")
            };
        }
        if let Some(constant) = schema.get("const") {
            return literal_expression_of(constant);
        }
        if let Some(variants) = schema["enum"].as_array() {
            return enum_expression_of(variants);
        }
        for union_keyword in ["oneOf", "anyOf"] {
            if let Some(variants) = schema[union_keyword].as_array() {
                let union = self.union_expression_of(variants, indent_level);
                return if schema.get("properties").is_some() {
                    format!(
                        "z.intersection({}, {union})",
                        self.object_expression_of(schema, indent_level)
                    )
                } else {
                    union
                };
            }
        }
        if let Some(parts) = schema["allOf"].as_array() {
            let mut part_expressions = parts
                .iter()
                .map(|part| self.expression_of(part, indent_level));
            let first = part_expressions
                .next()
                .unwrap_or_else(|| "z.unknown()".to_owned());
            return part_expressions.fold(first, |intersection, part| {
                format!("z.intersection({intersection}, {part})")
            });
        }
        match &schema["type"] {
            Value::String(type_name) => {
                self.expression_of_a_json_type(type_name, schema, indent_level)
            }
            Value::Array(type_names) => {
                let type_names: Vec<&str> = type_names.iter().filter_map(Value::as_str).collect();
                let not_null: Vec<&str> = type_names
                    .iter()
                    .copied()
                    .filter(|type_name| *type_name != "null")
                    .collect();
                let accepts_null = not_null.len() < type_names.len();
                let not_null_expression = match not_null.as_slice() {
                    [] => return "z.null()".to_owned(),
                    [one] => self.expression_of_a_json_type(one, schema, indent_level),
                    several => format!(
                        "z.union([{}])",
                        several
                            .iter()
                            .map(|type_name| self.expression_of_a_json_type(
                                type_name,
                                schema,
                                indent_level
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                if accepts_null {
                    format!("{not_null_expression}.nullable()")
                } else {
                    not_null_expression
                }
            }
            _ if schema.get("properties").is_some() => {
                self.object_expression_of(schema, indent_level)
            }
            _ => "z.unknown()".to_owned(),
        }
    }

    fn union_expression_of(&self, variants: &[Value], indent_level: usize) -> String {
        let variant_expressions: Vec<String> = variants
            .iter()
            .map(|variant| self.expression_of(variant, indent_level))
            .collect();
        match variant_expressions.as_slice() {
            [one] => one.clone(),
            [some, null] | [null, some] if null == "z.null()" => format!("{some}.nullable()"),
            several => format!("z.union([{}])", several.join(", ")),
        }
    }

    fn expression_of_a_json_type(
        &self,
        type_name: &str,
        schema: &Value,
        indent_level: usize,
    ) -> String {
        let minimum = schema["minimum"].as_f64();
        match type_name {
            "string" if schema["format"] == "uuid" => "z.guid()".to_owned(),
            "string" => "z.string()".to_owned(),
            "integer" if is_a_wide_integer_format(schema) => {
                let (number_minimum, bigint_minimum) = match minimum {
                    Some(minimum) if minimum.fract() == 0.0 => {
                        (format!(".min({minimum})"), format!(".min({minimum}n)"))
                    }
                    _ => (String::new(), String::new()),
                };
                format!("z.union([z.number().int(){number_minimum}, z.bigint(){bigint_minimum}])")
            }
            "integer" => format!(
                "z.number().int(){}",
                minimum.map_or_else(String::new, |minimum| format!(".min({minimum})"))
            ),
            "number" => format!(
                "z.number(){}",
                minimum.map_or_else(String::new, |minimum| format!(".min({minimum})"))
            ),
            "boolean" => "z.boolean()".to_owned(),
            "null" => "z.null()".to_owned(),
            "array" => self.array_expression_of(schema, indent_level),
            "object" => self.object_expression_of(schema, indent_level),
            _ => "z.unknown()".to_owned(),
        }
    }

    fn array_expression_of(&self, schema: &Value, indent_level: usize) -> String {
        let item_expression = schema.get("items").map_or_else(
            || "z.unknown()".to_owned(),
            |items| self.expression_of(items, indent_level),
        );
        match (schema["minItems"].as_u64(), schema["maxItems"].as_u64()) {
            (Some(min_items), Some(max_items)) if min_items == max_items && max_items <= 8 => {
                let tuple_items =
                    vec![item_expression; usize::try_from(max_items).unwrap_or_default()];
                format!("z.tuple([{}])", tuple_items.join(", "))
            }
            (min_items, max_items) => format!(
                "z.array({item_expression}){}{}",
                min_items.map_or_else(String::new, |min_items| format!(".min({min_items})")),
                max_items.map_or_else(String::new, |max_items| format!(".max({max_items})")),
            ),
        }
    }

    fn object_expression_of(&self, schema: &Value, indent_level: usize) -> String {
        let properties = schema["properties"].as_object();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|required| required.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let additional_properties = schema.get("additionalProperties");
        if properties.is_none_or(Map::is_empty) {
            return match additional_properties {
                Some(Value::Bool(false)) => "z.strictObject({})".to_owned(),
                Some(Value::Bool(true)) | None => "z.record(z.string(), z.unknown())".to_owned(),
                Some(value_schema) => format!(
                    "z.record(z.string(), {})",
                    self.expression_of(value_schema, indent_level)
                ),
            };
        }
        let member_indent = "  ".repeat(indent_level + 1);
        let mut shape = String::from("{\n");
        for (property_name, property_schema) in properties.into_iter().flatten() {
            let optional = if required.contains(&property_name.as_str()) {
                ""
            } else {
                ".optional()"
            };
            let _ = writeln!(
                shape,
                "{member_indent}{}: {}{optional},",
                property_key(property_name),
                self.expression_of(property_schema, indent_level + 1)
            );
        }
        let _ = write!(shape, "{}}}", "  ".repeat(indent_level));
        match additional_properties {
            Some(Value::Bool(false)) => format!("z.strictObject({shape})"),
            Some(Value::Bool(true)) | None => format!("z.looseObject({shape})"),
            Some(value_schema) => format!(
                "z.object({shape}).catchall({})",
                self.expression_of(value_schema, indent_level)
            ),
        }
    }
}

fn literal_expression_of(constant: &Value) -> String {
    match constant {
        Value::Null => "z.null()".to_owned(),
        constant => format!("z.literal({})", json_text_of(constant)),
    }
}

fn enum_expression_of(variants: &[Value]) -> String {
    if !variants.is_empty() && variants.iter().all(Value::is_string) {
        return format!(
            "z.enum([{}])",
            variants
                .iter()
                .map(json_text_of)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    match variants {
        [one] => literal_expression_of(one),
        several => format!(
            "z.union([{}])",
            several
                .iter()
                .map(literal_expression_of)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn json_text_of(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| unreachable!("a JSON value always serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn expression_outside_any_component(schema: &Value) -> String {
        let no_components = BTreeMap::new();
        ZodSourceWriter {
            declaration_position_by_name: &no_components,
            position_being_declared: None,
        }
        .expression_of(schema, 0)
    }

    #[test]
    fn a_closed_object_is_strict_and_an_open_one_keeps_its_unknown_fields() {
        assert_eq!(
            expression_outside_any_component(&json!({
                "type": "object",
                "properties": {
                    "project_directory": { "type": "string" },
                    "author": { "anyOf": [{ "$ref": "#/components/schemas/Author" }, { "type": "null" }] }
                },
                "required": ["project_directory"],
                "additionalProperties": false
            })),
            "z.strictObject({\n  project_directory: z.string(),\n  author: Author.nullable().optional(),\n})"
        );
        assert_eq!(
            expression_outside_any_component(&json!({
                "type": "object",
                "properties": { "action": { "type": "string" } },
                "required": ["action"]
            })),
            "z.looseObject({\n  action: z.string(),\n})"
        );
    }

    #[test]
    fn a_wide_integer_takes_a_bigint_and_a_narrow_one_only_a_number() {
        assert_eq!(
            expression_outside_any_component(
                &json!({ "type": ["integer", "null"], "format": "uint64", "minimum": 0 })
            ),
            "z.union([z.number().int().min(0), z.bigint().min(0n)]).nullable()"
        );
        assert_eq!(
            expression_outside_any_component(
                &json!({ "type": "integer", "format": "uint32", "minimum": 0 })
            ),
            "z.number().int().min(0)"
        );
    }

    #[test]
    fn a_uuid_string_is_checked_as_one_and_any_other_string_is_not() {
        assert_eq!(
            expression_outside_any_component(
                &json!({ "type": ["string", "null"], "format": "uuid" })
            ),
            "z.guid().nullable()"
        );
        assert_eq!(
            expression_outside_any_component(&json!({ "type": "string" })),
            "z.string()"
        );
    }

    #[test]
    fn a_component_naming_one_declared_later_reads_it_lazily() {
        let component_schemas = json!({
            "Tree": { "type": "object", "properties": { "children": { "type": "array", "items": { "$ref": "#/components/schemas/Tree" } } } },
            "Leaf": { "type": "string" },
            "Branch": { "type": "object", "properties": { "leaf": { "$ref": "#/components/schemas/Leaf" } } }
        });
        let component_schemas = component_schemas.as_object().unwrap();
        let source = zod_schemas_javascript(component_schemas, &[], &[]);
        assert!(source.find("const Leaf =").unwrap() < source.find("const Branch =").unwrap());
        assert!(source.contains("leaf: Leaf.optional(),"), "{source}");
        assert!(
            source.contains("children: z.array(z.lazy(() => Tree)).optional(),"),
            "{source}"
        );
    }
}
