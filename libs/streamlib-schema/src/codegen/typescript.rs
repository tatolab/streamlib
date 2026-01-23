// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! TypeScript code generation from schema definitions.

use crate::definition::{Field, FieldType, SchemaDefinition};
use crate::error::Result;

/// Generate TypeScript code for a schema definition.
pub fn generate_typescript(schema: &SchemaDefinition) -> Result<String> {
    let mut code = String::new();

    // File header
    code.push_str(&format!(
        r#"// Generated from {}
// DO NOT EDIT - regenerate with `streamlib schema sync`

import * as msgpack from "@msgpack/msgpack";

"#,
        schema.full_name()
    ));

    // Generate nested interfaces first (depth-first)
    let nested_interfaces =
        generate_nested_interfaces(schema, &schema.fields, &schema.rust_struct_name());
    code.push_str(&nested_interfaces);

    // Generate main interface
    let main_interface =
        generate_interface(schema, &schema.fields, &schema.rust_struct_name(), true);
    code.push_str(&main_interface);

    Ok(code)
}

/// Generate nested interfaces for object fields.
fn generate_nested_interfaces(
    schema: &SchemaDefinition,
    fields: &[Field],
    parent_name: &str,
) -> String {
    let mut code = String::new();

    for field in fields {
        if matches!(field.field_type, FieldType::Complex(ref s) if s.to_lowercase() == "object") {
            let nested_name = format!("{}{}", parent_name, to_pascal_case(&field.name));

            // Recursively generate any deeper nested interfaces
            let deeper_nested = generate_nested_interfaces(schema, &field.fields, &nested_name);
            code.push_str(&deeper_nested);

            // Generate this nested interface
            let nested_interface = generate_interface(schema, &field.fields, &nested_name, false);
            code.push_str(&nested_interface);
        }
    }

    code
}

/// Generate a single class with msgpack support.
fn generate_interface(
    schema: &SchemaDefinition,
    fields: &[Field],
    class_name: &str,
    is_main: bool,
) -> String {
    let mut code = String::new();

    // JSDoc comment (only for main class)
    if is_main {
        if let Some(ref desc) = schema.description {
            code.push_str(&format!("/** {} */\n", desc));
        }
    }

    // Export class
    code.push_str(&format!("export class {} {{\n", class_name));

    // Collect field info for constructor
    let mut field_params = Vec::new();
    let mut field_assignments = Vec::new();

    // Fields as public properties
    for field in fields {
        let field_name = to_camel_case(&field.name);
        let nested_class_name = if matches!(field.field_type, FieldType::Complex(ref s) if s.to_lowercase() == "object")
        {
            Some(format!("{}{}", class_name, to_pascal_case(&field.name)))
        } else {
            None
        };

        let ts_type = field
            .field_type
            .to_typescript_type(nested_class_name.as_deref());

        // Use original name for serde compatibility
        let prop_name = if field_name != field.name {
            field.name.clone()
        } else {
            field_name.clone()
        };

        // Field with JSDoc comment
        if let Some(ref desc) = field.description {
            code.push_str(&format!("    /** {} */\n", desc));
        }
        code.push_str(&format!("    public {}: {};\n", prop_name, ts_type));

        field_params.push(format!("{}: {}", prop_name, ts_type));
        field_assignments.push(format!("        this.{} = {};", prop_name, prop_name));
    }

    code.push('\n');

    // Constructor
    code.push_str(&format!(
        "    constructor({}) {{\n",
        field_params.join(", ")
    ));
    for assignment in &field_assignments {
        code.push_str(assignment);
        code.push('\n');
    }
    code.push_str("    }\n\n");

    // Static fromMsgpack method
    code.push_str(&format!(
        r#"    static fromMsgpack(data: Uint8Array): {} {{
        const obj = msgpack.decode(data) as any;
        return new {}({});
    }}

"#,
        class_name,
        class_name,
        fields
            .iter()
            .map(|f| {
                let field_name = to_camel_case(&f.name);
                let prop_name = if field_name != f.name {
                    f.name.clone()
                } else {
                    field_name
                };
                format!("obj.{}", prop_name)
            })
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Instance toMsgpack method
    code.push_str(&format!(
        r#"    toMsgpack(): Uint8Array {{
        return msgpack.encode({{ {} }});
    }}
"#,
        fields
            .iter()
            .map(|f| {
                let field_name = to_camel_case(&f.name);
                let prop_name = if field_name != f.name {
                    f.name.clone()
                } else {
                    field_name
                };
                format!("{}: this.{}", prop_name, prop_name)
            })
            .collect::<Vec<_>>()
            .join(", ")
    ));

    code.push_str("}\n\n");
    code
}

/// Convert string to PascalCase.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Convert string to camelCase.
fn to_camel_case(s: &str) -> String {
    let pascal = to_pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Generate the index.ts file for a collection of schemas.
pub fn generate_index_ts(schemas: &[SchemaDefinition]) -> String {
    let mut code = String::new();

    code.push_str("// Generated by streamlib schema sync\n");
    code.push_str("// DO NOT EDIT\n\n");

    // Re-exports
    for schema in schemas {
        let module_name = schema.rust_module_name();
        let interface_name = schema.rust_struct_name();
        code.push_str(&format!(
            "export {{ {} }} from './{}';\n",
            interface_name, module_name
        ));
    }

    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_yaml;

    #[test]
    fn test_generate_simple_interface() {
        let yaml = r#"
name: com.tatolab.videoframe
version: 1.0.0
description: "Video frame"

fields:
  - name: surface_id
    type: uint64
    description: "GPU surface ID"
  - name: width
    type: uint32
  - name: height
    type: uint32
  - name: timestamp_ns
    type: int64
"#;

        let schema = parse_yaml(yaml).unwrap();
        let code = generate_typescript(&schema).unwrap();

        assert!(code.contains("export class Videoframe {"));
        assert!(code.contains("public surface_id: number;"));
        assert!(code.contains("public width: number;"));
        assert!(code.contains("public timestamp_ns: number;"));
        assert!(code.contains("/** GPU surface ID */"));
        assert!(code.contains("static fromMsgpack(data: Uint8Array)"));
        assert!(code.contains("toMsgpack(): Uint8Array"));
    }

    #[test]
    fn test_generate_nested_interface() {
        let yaml = r#"
name: com.example.detection
version: 1.0.0

fields:
  - name: label
    type: string
  - name: confidence
    type: float32
  - name: bounding_box
    type: object
    fields:
      - name: x
        type: uint32
      - name: y
        type: uint32
      - name: width
        type: uint32
      - name: height
        type: uint32
"#;

        let schema = parse_yaml(yaml).unwrap();
        let code = generate_typescript(&schema).unwrap();

        assert!(code.contains("export class Detection {"));
        assert!(code.contains("export class DetectionBoundingBox {"));
        assert!(code.contains("public bounding_box: DetectionBoundingBox;"));
    }

    #[test]
    fn test_generate_complex_types() {
        let yaml = r#"
name: com.example.complex
version: 1.0.0

fields:
  - name: tags
    type: array<string>
  - name: metadata
    type: map<string,int32>
  - name: optional_value
    type: optional<float64>
"#;

        let schema = parse_yaml(yaml).unwrap();
        let code = generate_typescript(&schema).unwrap();

        assert!(code.contains("public tags: string[];"));
        assert!(code.contains("public metadata: Record<string, number>;"));
        assert!(code.contains("public optional_value: number | null;"));
    }

    #[test]
    fn test_generate_index_ts() {
        let schemas = vec![
            parse_yaml(
                r#"
name: com.tatolab.videoframe
version: 1.0.0
"#,
            )
            .unwrap(),
            parse_yaml(
                r#"
name: com.tatolab.audioframe
version: 1.0.0
"#,
            )
            .unwrap(),
        ];

        let code = generate_index_ts(&schemas);

        assert!(code.contains("export { Videoframe } from './com_tatolab_videoframe';"));
        assert!(code.contains("export { Audioframe } from './com_tatolab_audioframe';"));
    }

    #[test]
    fn test_to_camel_case() {
        assert_eq!(to_camel_case("video_frame"), "videoFrame");
        assert_eq!(to_camel_case("VideoFrame"), "videoFrame");
        assert_eq!(to_camel_case("surface_id"), "surfaceId");
    }
}
