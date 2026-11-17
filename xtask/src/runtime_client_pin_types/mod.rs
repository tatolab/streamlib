// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The runtime client's `pin_types.d.ts` and `pin_types.js`: the built-in pin types, the
//! components that state them and the type of each stored value, written from the contract
//! file's `x-pin-types` and `components.schemas`. The two files import nothing, so an extension
//! and a browser page read them with no library.

use std::fmt::Write as _;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::contract_pin_types::{
    BuiltInPinTypeRow, PIN_ANNOTATIONS_COMPONENT_NAME, PIN_TYPE_COMPONENT_NAME,
    built_in_pin_type_rows, components_reached_from,
};
use crate::generate_runtime_client::{
    GENERATED_SOURCE_HEADER, json_string_literal, property_key, write_component_declaration,
};

/// The components the pin types file declares besides each stored value's.
const PIN_TYPE_ROOT_COMPONENT_NAMES: &[&str] = &[
    PIN_TYPE_COMPONENT_NAME,
    "DeclaredPinType",
    "PinFlow",
    PIN_ANNOTATIONS_COMPONENT_NAME,
    "PinDeclaration",
];

/// The pin types as the generated client package writes them.
pub(crate) struct RuntimeClientPinTypes<'contract> {
    rows: Vec<BuiltInPinTypeRow<'contract>>,
    /// Every component `pin_types.d.ts` declares, in the contract file's order.
    pub(crate) component_names: Vec<&'contract str>,
    component_schemas: &'contract Map<String, Value>,
}

impl<'contract> RuntimeClientPinTypes<'contract> {
    pub(crate) fn of(
        contract: &'contract Value,
        component_schemas: &'contract Map<String, Value>,
    ) -> Result<Self> {
        let rows = built_in_pin_type_rows(contract)?;
        let mut roots = PIN_TYPE_ROOT_COMPONENT_NAMES.to_vec();
        roots.extend(rows.iter().filter_map(|row| row.value_component));
        let component_names = components_reached_from(component_schemas, &roots)?;
        Ok(Self {
            rows,
            component_names,
            component_schemas,
        })
    }

    pub(crate) fn typescript_declarations(&self) -> String {
        let mut declarations = String::from(GENERATED_SOURCE_HEADER);
        declarations.push('\n');
        for name in &self.component_names {
            write_component_declaration(&mut declarations, name, &self.component_schemas[*name]);
        }
        let _ = writeln!(
            declarations,
            "/** The name of a built-in pin type: its `type` tag in a `PinType`. */\n\
             export type BuiltInPinTypeName = {};\n",
            self.rows
                .iter()
                .map(|row| json_string_literal(row.name))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        declarations.push_str(
            "/** An annotation a pin may state: a key of `PinAnnotations`. */\n\
             export type PinAnnotationName = keyof PinAnnotations;\n\n\
             /** The value of each built-in type that stores one. A type with no key stores none. */\n\
             export interface PinValues {\n",
        );
        for row in &self.rows {
            if let Some(value_component) = row.value_component {
                let _ = writeln!(
                    declarations,
                    "  readonly {}: {value_component};",
                    property_key(row.name)
                );
            }
        }
        declarations.push_str(
            "}\n\n\
             /** The stored value of the built-in type `Name`. */\n\
             export type PinValue<Name extends keyof PinValues> = PinValues[Name];\n\n\
             /** One built-in pin type, as the contract lists it. */\n\
             export interface PinTypeDescription {\n\
             \x20 /** Its name, its `type` tag in a `PinType`. */\n\
             \x20 readonly name: BuiltInPinTypeName;\n\
             \x20 /** What a person reads it as, such as `Encoded video`. */\n\
             \x20 readonly label: string;\n\
             \x20 /** What it carries. */\n\
             \x20 readonly documentation: string;\n\
             \x20 /** How a pin of it may read its messages. */\n\
             \x20 readonly flows: readonly PinFlow[];\n\
             \x20 /** What a pin of it may state besides its type. */\n\
             \x20 readonly annotations: readonly PinAnnotationName[];\n\
             \x20 /** Each spelling a Python declaration writes it with. */\n\
             \x20 readonly python_forms: readonly string[];\n\
             \x20 /** How a TypeScript declaration writes it. */\n\
             \x20 readonly typescript_form: string;\n\
             \x20 /** The contract component that states its stored value; absent, it stores none. */\n\
             \x20 readonly value_component?: string;\n\
             \x20 /** The contract version it was added in. */\n\
             \x20 readonly since: number;\n\
             \x20 /** Whether it may still change before it turns stable. */\n\
             \x20 readonly stability: \"stable\" | \"experimental\";\n\
             }\n\n\
             /** Every built-in pin type, in the contract's order. */\n\
             export declare const BUILT_IN_PIN_TYPES: readonly PinTypeDescription[];\n",
        );
        declarations
    }

    pub(crate) fn javascript(&self) -> String {
        let mut module = String::from(GENERATED_SOURCE_HEADER);
        module.push_str("\nexport const BUILT_IN_PIN_TYPES = Object.freeze([\n");
        for row in &self.rows {
            let texts = |items: &[&str]| -> String {
                format!(
                    "Object.freeze([{}])",
                    items
                        .iter()
                        .map(|item| json_string_literal(item))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            module.push_str("  Object.freeze({\n");
            let _ = writeln!(module, "    name: {},", json_string_literal(row.name));
            let _ = writeln!(module, "    label: {},", json_string_literal(row.label));
            let _ = writeln!(
                module,
                "    documentation: {},",
                json_string_literal(row.documentation)
            );
            let _ = writeln!(module, "    flows: {},", texts(&row.flows));
            let _ = writeln!(module, "    annotations: {},", texts(&row.annotations));
            let _ = writeln!(module, "    python_forms: {},", texts(&row.python_forms));
            let _ = writeln!(
                module,
                "    typescript_form: {},",
                json_string_literal(row.typescript_form)
            );
            if let Some(value_component) = row.value_component {
                let _ = writeln!(
                    module,
                    "    value_component: {},",
                    json_string_literal(value_component)
                );
            }
            let _ = writeln!(module, "    since: {},", row.since_contract_version);
            let _ = writeln!(
                module,
                "    stability: {},",
                json_string_literal(row.stability)
            );
            module.push_str("  }),\n");
        }
        module.push_str("]);\n");
        module
    }
}

#[cfg(test)]
mod tests;
