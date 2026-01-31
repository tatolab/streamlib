// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generate code from JTD schemas using jtd-codegen (Rust) or streamlib-schema (Python).
//!
//! Reads schema paths from `libs/streamlib/Cargo.toml` [package.metadata.streamlib]
//! or from a `pyproject.toml` [tool.streamlib] section.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::RuntimeTarget;

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

/// pyproject.toml structure for reading [tool.streamlib] metadata.
#[derive(Deserialize)]
struct PyProjectToml {
    tool: Option<PyProjectTool>,
}

#[derive(Deserialize)]
struct PyProjectTool {
    streamlib: Option<PyProjectStreamlib>,
}

#[derive(Deserialize)]
struct PyProjectStreamlib {
    /// Data type schemas (YAML format)
    #[serde(default)]
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

pub fn run(runtime: RuntimeTarget, output: Option<PathBuf>, source: Option<PathBuf>) -> Result<()> {
    let workspace_root = crate::workspace_root()?;

    match runtime {
        RuntimeTarget::Rust => run_rust_codegen(&workspace_root, output, source),
        RuntimeTarget::Python => run_python_codegen(&workspace_root, output, source),
    }
}

/// Read schema paths from a source file (Cargo.toml or pyproject.toml).
///
/// Returns (base_dir, schema_paths) where base_dir is the directory containing
/// the source file (schema paths are relative to it).
fn read_schema_paths(
    workspace_root: &Path,
    source: Option<PathBuf>,
) -> Result<(PathBuf, Vec<String>)> {
    let source_path = source.unwrap_or_else(|| workspace_root.join("libs/streamlib/Cargo.toml"));

    let source_filename = source_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    let base_dir = source_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| workspace_root.to_path_buf());

    println!("Reading schemas from: {}", source_path.display());

    let content = fs::read_to_string(&source_path)
        .with_context(|| format!("Failed to read {}", source_path.display()))?;

    let schemas = if source_filename == "pyproject.toml" {
        let pyproject: PyProjectToml =
            toml::from_str(&content).context("Failed to parse pyproject.toml")?;
        pyproject
            .tool
            .and_then(|t| t.streamlib)
            .map(|s| s.schemas)
            .unwrap_or_default()
    } else {
        let cargo_toml: CargoToml =
            toml::from_str(&content).context("Failed to parse Cargo.toml")?;
        cargo_toml
            .package
            .metadata
            .and_then(|m| m.streamlib)
            .map(|s| s.schemas)
            .unwrap_or_default()
    };

    Ok((base_dir, schemas))
}

// =============================================================================
// Rust codegen (existing behavior)
// =============================================================================

fn run_rust_codegen(
    workspace_root: &Path,
    output: Option<PathBuf>,
    source: Option<PathBuf>,
) -> Result<()> {
    // Verify jtd-codegen is available
    let jtd_check = Command::new("jtd-codegen").arg("--version").output();

    if jtd_check.is_err() {
        anyhow::bail!("jtd-codegen not found. Install with: cargo install jtd-codegen");
    }

    let (base_dir, schemas) = read_schema_paths(workspace_root, source)?;

    if schemas.is_empty() {
        println!("No schemas found");
        return Ok(());
    }

    println!("Found {} schemas", schemas.len());

    // Create output directory
    let output_dir =
        output.unwrap_or_else(|| workspace_root.join("libs/streamlib/src/_generated_"));
    fs::create_dir_all(&output_dir).context("Failed to create output directory")?;

    // Create temp directory for JSON conversion
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    // Process each schema
    let mut modules = Vec::new(); // (module_name, struct_name)

    for schema_path in &schemas {
        let full_path = base_dir.join(schema_path);
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
        let json_content =
            serde_json::to_string_pretty(&json_value).context("Failed to serialize to JSON")?;
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
        "Generated {} Rust modules in {}",
        modules.len(),
        output_dir.display()
    );

    Ok(())
}

// =============================================================================
// Python codegen
// =============================================================================

fn run_python_codegen(
    workspace_root: &Path,
    output: Option<PathBuf>,
    source: Option<PathBuf>,
) -> Result<()> {
    // Resolve schema YAML files.
    // When --source is provided, read schema list from that file.
    // Otherwise, scan libs/streamlib-schema/schemas/*.yaml for core data type schemas.
    let schema_files: Vec<PathBuf> = if let Some(ref source_path) = source {
        let (base_dir, schema_list) = read_schema_paths(workspace_root, Some(source_path.clone()))?;
        schema_list.iter().map(|p| base_dir.join(p)).collect()
    } else {
        // Default: scan streamlib-schema/schemas/ for core data type schemas
        let schemas_dir = workspace_root.join("libs/streamlib-schema/schemas");
        println!("Reading schemas from: {}", schemas_dir.display());
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&schemas_dir)
            .with_context(|| format!("Failed to read {}", schemas_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                files.push(path);
            }
        }
        files.sort();
        files
    };

    if schema_files.is_empty() {
        println!("No schemas found");
        return Ok(());
    }

    println!("Found {} schemas", schema_files.len());

    // Clean and recreate output directory to remove stale files
    let output_dir = output
        .unwrap_or_else(|| workspace_root.join("libs/streamlib-python/python/streamlib/schemas"));
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir).context("Failed to clean output directory")?;
    }
    fs::create_dir_all(&output_dir).context("Failed to create output directory")?;

    // Parse all schemas using streamlib-schema
    let mut parsed_schemas = Vec::new();

    for full_path in &schema_files {
        println!("  Processing: {}", full_path.display());

        let schema = streamlib_schema::parse_yaml_file(full_path)
            .with_context(|| format!("Failed to parse schema {}", full_path.display()))?;

        parsed_schemas.push(schema);
    }

    // Generate Python files for each schema.
    // Use schema_name_to_struct_name() for correct class names (matches Rust _generated_).
    // The codegen library uses rust_struct_name() which returns "Config" for all config schemas.
    // We post-process to rename to the correct name (e.g., "CameraConfig").
    let mut modules: Vec<(String, String)> = Vec::new(); // (module_name, class_name)

    for schema in &parsed_schemas {
        let module_name = schema_name_to_module_name(&schema.name);
        let correct_class_name = schema_name_to_struct_name(&schema.name);
        let codegen_class_name = schema.rust_struct_name();

        let mut python_code = streamlib_schema::codegen::generate_python(schema)
            .with_context(|| format!("Failed to generate Python for {}", schema.full_name()))?;

        // Rename class if the codegen name differs from the correct name
        if codegen_class_name != correct_class_name {
            python_code = python_code.replace(
                &format!("class {}:", codegen_class_name),
                &format!("class {}:", correct_class_name),
            );
            python_code = python_code.replace(
                &format!("-> \"{}\":", codegen_class_name),
                &format!("-> \"{}\":", correct_class_name),
            );
            // Also rename nested class prefixes (e.g., "ConfigWhip" -> "WebrtcWhipConfigWhip")
            // Nested classes use the parent class name as prefix
            for field in &schema.fields {
                if matches!(field.field_type, streamlib_schema::definition::FieldType::Complex(ref s) if s.eq_ignore_ascii_case("object"))
                {
                    let old_nested =
                        format!("{}{}", codegen_class_name, to_pascal_case(&field.name));
                    let new_nested =
                        format!("{}{}", correct_class_name, to_pascal_case(&field.name));
                    if old_nested != new_nested {
                        python_code = python_code.replace(&old_nested, &new_nested);
                    }
                }
            }
        }

        let output_path = output_dir.join(format!("{}.py", module_name));
        fs::write(&output_path, &python_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        println!("    → {}.py (class {})", module_name, correct_class_name);
        modules.push((module_name, correct_class_name));
    }

    // Generate __init__.py with correct class names
    let mut init_py = String::from("# Generated by streamlib schema sync\n# DO NOT EDIT\n\n");
    for (module_name, class_name) in &modules {
        init_py.push_str(&format!("from .{} import {}\n", module_name, class_name));
    }
    init_py.push_str("\n__all__ = [\n");
    for (_, class_name) in &modules {
        init_py.push_str(&format!("    \"{}\",\n", class_name));
    }
    init_py.push_str("]\n");

    let init_path = output_dir.join("__init__.py");
    fs::write(&init_path, &init_py).context("Failed to write __init__.py")?;

    println!(
        "Generated {} Python modules in {}",
        modules.len(),
        output_dir.display()
    );

    Ok(())
}

// =============================================================================
// Rust post-processing (unchanged)
// =============================================================================

/// Post-process jtd-codegen output to add derives, make fields pub, etc.
fn post_process_generated_code(code: &str, _struct_name: &str) -> Result<String> {
    let mut result = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\n\
         //! Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n",
    );

    // Track whether we're inside an enum and if we've marked the first variant
    let mut in_enum = false;
    let mut marked_first_variant = false;

    for line in code.lines() {
        // Skip the chrono import if unused (jtd-codegen adds it but we may not need it)
        if line.starts_with("use chrono::") {
            continue;
        }

        // Add Clone, PartialEq, Default to derives for both enums and structs
        if line.contains("#[derive(Debug, Serialize, Deserialize)]") {
            result
                .push_str("#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]\n");
            continue;
        }

        // Track enum boundaries
        if line.starts_with("pub enum ") {
            in_enum = true;
            marked_first_variant = false;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // Add #[default] to first enum variant
        if in_enum && !marked_first_variant {
            let trimmed = line.trim();
            // Check if this is a variant (starts with uppercase letter after any #[serde] attr)
            if !trimmed.is_empty()
                && !trimmed.starts_with("#")
                && !trimmed.starts_with("//")
                && !trimmed.starts_with("}")
            {
                // This is the first variant - add #[default] before it
                result.push_str("    #[default]\n");
                marked_first_variant = true;
            }
        }

        // End of enum
        if in_enum && line.trim() == "}" {
            in_enum = false;
        }

        // Make fields public
        if line.starts_with("    ")
            && !line.trim().starts_with("#")
            && !line.trim().starts_with("//")
        {
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

// =============================================================================
// Name conversion helpers
// =============================================================================

/// Convert schema name to struct name (PascalCase).
/// e.g., "com.tatolab.audioframe.1ch" -> "Audioframe1Ch"
fn schema_name_to_struct_name(name: &str) -> String {
    // Remove version suffix if present (e.g., "@1.0.0")
    let name = name.split('@').next().unwrap_or(name);

    // Get last segment
    let last_segment = name.split('.').next_back().unwrap_or(name);

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
            return format!("{}{}", to_pascal_case(parent), to_pascal_case(last_segment));
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
        content.push_str(&format!("pub mod {};\n", module_name));
    }

    content.push('\n');

    for (module_name, struct_name) in modules {
        content.push_str(&format!("pub use {}::{};\n", module_name, struct_name));
    }

    content
}
