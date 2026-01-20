// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generate Rust structs from JTD schemas using jtd-codegen.
//!
//! Reads schema paths from `libs/streamlib/Cargo.toml` [package.metadata.streamlib]
//! and generates Rust code in `libs/streamlib/src/_generated_/`.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::process::Command;

/// Cargo.toml structure for reading metadata.
#[derive(Deserialize)]
struct CargoToml {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    metadata: Option<Metadata>,
}

#[derive(Deserialize)]
struct Metadata {
    streamlib: Option<StreamlibMetadata>,
}

#[derive(Deserialize)]
struct StreamlibMetadata {
    /// Data type and config schemas (JTD format)
    schemas: Vec<String>,
}

/// Minimal JTD schema structure for extracting metadata.
#[derive(Debug, Deserialize)]
struct JtdSchema {
    metadata: JtdMetadata,
}

#[derive(Debug, Deserialize)]
struct JtdMetadata {
    name: String,
}

pub fn run() -> Result<()> {
    // Verify jtd-codegen is available
    let jtd_check = Command::new("jtd-codegen")
        .arg("--version")
        .output();

    if jtd_check.is_err() {
        anyhow::bail!(
            "jtd-codegen not found. Install with: cargo install jtd-codegen"
        );
    }

    let workspace_root = crate::workspace_root()?;
    let streamlib_dir = workspace_root.join("libs/streamlib");
    let cargo_toml_path = streamlib_dir.join("Cargo.toml");

    println!("Reading schemas from: {}", cargo_toml_path.display());

    // Read and parse Cargo.toml
    let cargo_toml_content = fs::read_to_string(&cargo_toml_path)
        .context("Failed to read libs/streamlib/Cargo.toml")?;
    let cargo_toml: CargoToml =
        toml::from_str(&cargo_toml_content).context("Failed to parse Cargo.toml")?;

    let schemas = cargo_toml
        .package
        .metadata
        .and_then(|m| m.streamlib)
        .map(|s| s.schemas)
        .unwrap_or_default();

    if schemas.is_empty() {
        println!("No schemas found in [package.metadata.streamlib]");
        return Ok(());
    }

    println!("Found {} schemas", schemas.len());

    // Create output directory
    let output_dir = streamlib_dir.join("src/_generated_");
    fs::create_dir_all(&output_dir).context("Failed to create _generated_ directory")?;

    // Create temp directory for JSON conversion
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    // Process each schema
    let mut modules = Vec::new(); // (module_name, struct_name)

    for schema_path in &schemas {
        let full_path = streamlib_dir.join(schema_path);
        println!("  Processing: {}", schema_path);

        // Read YAML and extract metadata
        let yaml_content = fs::read_to_string(&full_path)
            .with_context(|| format!("Failed to read {}", schema_path))?;

        let schema: JtdSchema = serde_yaml::from_str(&yaml_content)
            .with_context(|| format!("Failed to parse {}", schema_path))?;

        // Derive names from schema metadata
        let module_name = schema_name_to_module_name(&schema.metadata.name);
        let struct_name = schema_name_to_struct_name(&schema.metadata.name);

        // Convert YAML to JSON
        let json_value: serde_json::Value = serde_yaml::from_str(&yaml_content)
            .with_context(|| format!("Failed to parse YAML {}", schema_path))?;

        // Write JSON to temp file (jtd-codegen uses filename for struct name)
        let json_filename = format!("{}.json", struct_name);
        let json_path = temp_dir.path().join(&json_filename);
        let json_content = serde_json::to_string_pretty(&json_value)
            .context("Failed to serialize to JSON")?;
        fs::write(&json_path, &json_content)
            .with_context(|| format!("Failed to write {}", json_path.display()))?;

        // Create temp output dir for this schema
        let temp_rust_out = temp_dir.path().join(format!("rust_{}", module_name));
        fs::create_dir_all(&temp_rust_out)?;

        // Run jtd-codegen
        let output = Command::new("jtd-codegen")
            .arg("--rust-out")
            .arg(&temp_rust_out)
            .arg("--")
            .arg(&json_path)
            .output()
            .context("Failed to run jtd-codegen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jtd-codegen failed for {}: {}", schema_path, stderr);
        }

        // Read generated code and post-process
        let generated_mod = temp_rust_out.join("mod.rs");
        let generated_code = fs::read_to_string(&generated_mod)
            .with_context(|| format!("Failed to read generated code for {}", schema_path))?;

        // Post-process: add derives, make fields pub, add copyright header
        let processed_code = post_process_generated_code(&generated_code, &struct_name)?;

        // Write to final location
        let output_path = output_dir.join(format!("{}.rs", module_name));
        fs::write(&output_path, processed_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        modules.push((module_name, struct_name));
    }

    // Generate mod.rs
    let mod_rs = generate_mod_rs(&modules);
    let mod_path = output_dir.join("mod.rs");
    fs::write(&mod_path, mod_rs).context("Failed to write mod.rs")?;

    println!(
        "Generated {} modules in {}",
        modules.len(),
        output_dir.display()
    );

    Ok(())
}

/// Post-process jtd-codegen output to add derives, make fields pub, etc.
fn post_process_generated_code(code: &str, _struct_name: &str) -> Result<String> {
    let mut result = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\n\
         //! Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n",
    );

    // Check if this is an enum (enums can't derive Default without #[default] attribute)
    let is_enum = code.contains("pub enum ");

    for line in code.lines() {
        // Skip the chrono import if unused (jtd-codegen adds it but we may not need it)
        if line.starts_with("use chrono::") {
            continue;
        }

        // Add Clone, PartialEq to derives (and Default only for structs)
        if line.contains("#[derive(Debug, Serialize, Deserialize)]") {
            if is_enum {
                result.push_str("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n");
            } else {
                result.push_str("#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]\n");
            }
            continue;
        }

        // Make fields public
        if line.starts_with("    ") && !line.trim().starts_with("#") && !line.trim().starts_with("//") {
            // Check if it's a field declaration (contains a colon but not in an attribute)
            let trimmed = line.trim();
            if trimmed.contains(": ") && !trimmed.starts_with("#") && !trimmed.starts_with("pub ") {
                result.push_str(&line.replacen("    ", "    pub ", 1));
                result.push('\n');
                continue;
            }
        }

        result.push_str(line);
        result.push('\n');
    }

    Ok(result)
}

/// Convert schema name to struct name (PascalCase).
/// e.g., "com.tatolab.audioframe.1ch" -> "Audioframe1Ch"
fn schema_name_to_struct_name(name: &str) -> String {
    // Remove version suffix if present (e.g., "@1.0.0")
    let name = name.split('@').next().unwrap_or(name);

    // Get last segment
    let last_segment = name.split('.').last().unwrap_or(name);

    // Handle special case for "config" suffix
    if last_segment == "config" {
        let segments: Vec<&str> = name.split('.').collect();
        if segments.len() >= 2 {
            let processor_name = segments[segments.len() - 2];
            return format!("{}Config", to_pascal_case(processor_name));
        }
    }

    // Handle channel suffixes like "1ch", "2ch" - use parent segment + suffix
    if last_segment
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        let segments: Vec<&str> = name.split('.').collect();
        if segments.len() >= 2 {
            let parent = segments[segments.len() - 2];
            return format!(
                "{}{}",
                to_pascal_case(parent),
                to_pascal_case(last_segment)
            );
        }
    }

    to_pascal_case(last_segment)
}

/// Convert schema name to module name (full schema name with underscores).
/// e.g., "com.tatolab.audioframe.1ch" -> "com_tatolab_audioframe_1ch"
fn schema_name_to_module_name(name: &str) -> String {
    // Remove version suffix if present (e.g., "@1.0.0")
    let name = name.split('@').next().unwrap_or(name);

    // Replace dots with underscores and convert to lowercase
    name.replace('.', "_").to_lowercase()
}

/// Convert to PascalCase.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' {
            capitalize_next = true;
        } else if c.is_ascii_digit() {
            result.push(c);
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

/// Generate mod.rs that exports all modules.
fn generate_mod_rs(modules: &[(String, String)]) -> String {
    let mut content = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\n\
         //! Generated schema types. DO NOT EDIT.\n\n",
    );

    for (module_name, _) in modules {
        content.push_str(&format!("mod {};\n", module_name));
    }

    content.push('\n');

    for (module_name, struct_name) in modules {
        content.push_str(&format!("pub use {}::{};\n", module_name, struct_name));
    }

    content
}
