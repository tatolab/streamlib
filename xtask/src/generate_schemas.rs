// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generate code from JTD schemas using jtd-codegen for all languages.
//!
//! All three languages (Rust, Python, TypeScript) use the same pipeline:
//! 1. Read schema YAML files
//! 2. Convert YAML → JSON
//! 3. Call jtd-codegen with the appropriate --{language}-out flag
//! 4. Post-process output (copyright headers, derives, etc.)

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

pub fn run(
    runtime: RuntimeTarget,
    output: PathBuf,
    project_file: Option<PathBuf>,
    schema_file: Option<PathBuf>,
    schema_dir: Option<PathBuf>,
) -> Result<()> {
    let workspace_root = crate::workspace_root()?;

    // Resolve input schemas
    let schema_files =
        resolve_schema_files(&workspace_root, project_file, schema_file, schema_dir)?;

    if schema_files.is_empty() {
        println!("No schemas found");
        return Ok(());
    }

    println!("Found {} schemas", schema_files.len());

    match runtime {
        RuntimeTarget::Rust => run_jtd_codegen_rust(&schema_files, &output),
        RuntimeTarget::Python => run_jtd_codegen_python(&schema_files, &output),
        RuntimeTarget::Typescript => run_jtd_codegen_typescript(&schema_files, &output),
    }
}

/// Resolve schema YAML files from one of three input modes.
fn resolve_schema_files(
    workspace_root: &Path,
    project_file: Option<PathBuf>,
    schema_file: Option<PathBuf>,
    schema_dir: Option<PathBuf>,
) -> Result<Vec<PathBuf>> {
    if let Some(project_path) = project_file {
        // Read schema list from project file (Cargo.toml or pyproject.toml)
        let (base_dir, schema_list) = read_schema_paths(workspace_root, project_path)?;
        Ok(schema_list.iter().map(|p| base_dir.join(p)).collect())
    } else if let Some(file_path) = schema_file {
        // Single schema file
        if !file_path.exists() {
            anyhow::bail!("Schema file not found: {}", file_path.display());
        }
        Ok(vec![file_path])
    } else if let Some(dir_path) = schema_dir {
        // Directory of schema files
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&dir_path)
            .with_context(|| format!("Failed to read directory {}", dir_path.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    } else {
        anyhow::bail!("No input specified. Use --project-file, --schema-file, or --schema-dir");
    }
}

/// Read schema paths from a project file (Cargo.toml or pyproject.toml).
fn read_schema_paths(
    workspace_root: &Path,
    source_path: PathBuf,
) -> Result<(PathBuf, Vec<String>)> {
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
// jtd-codegen pipeline (shared across all languages)
// =============================================================================

/// Verify jtd-codegen v0.4.1+ is available.
fn verify_jtd_codegen() -> Result<()> {
    let output = Command::new("jtd-codegen")
        .arg("--version")
        .output()
        .context("jtd-codegen not found. Install v0.4.1 from https://github.com/jsontypedef/json-typedef-codegen/releases/tag/v0.4.1")?;

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !version.contains("0.4") {
        anyhow::bail!(
            "jtd-codegen version {} found, but v0.4.1 is required (need --python-out and --typescript-out support)",
            version
        );
    }

    Ok(())
}

/// Convert a schema YAML file to JSON and extract metadata.
fn prepare_schema(yaml_path: &Path, temp_dir: &Path) -> Result<(String, String, PathBuf)> {
    let yaml_content = fs::read_to_string(yaml_path)
        .with_context(|| format!("Failed to read {}", yaml_path.display()))?;

    let schema: JtdSchema = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse {}", yaml_path.display()))?;

    let module_name = schema_name_to_module_name(&schema.metadata.name);
    let struct_name = schema_name_to_struct_name(&schema.metadata.name);

    // Convert YAML to JSON
    let json_value: serde_json::Value = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse YAML {}", yaml_path.display()))?;

    let json_filename = format!("{}.json", struct_name);
    let json_path = temp_dir.join(&json_filename);
    let json_content =
        serde_json::to_string_pretty(&json_value).context("Failed to serialize to JSON")?;
    fs::write(&json_path, &json_content)
        .with_context(|| format!("Failed to write {}", json_path.display()))?;

    Ok((module_name, struct_name, json_path))
}

// =============================================================================
// Rust codegen
// =============================================================================

fn run_jtd_codegen_rust(schema_files: &[PathBuf], output_dir: &Path) -> Result<()> {
    verify_jtd_codegen()?;

    fs::create_dir_all(output_dir).context("Failed to create output directory")?;
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    let mut modules = Vec::new();

    for yaml_path in schema_files {
        println!("  Processing: {}", yaml_path.display());

        let (module_name, struct_name, json_path) = prepare_schema(yaml_path, temp_dir.path())?;

        // Create temp output dir for this schema
        let temp_rust_out = temp_dir.path().join(format!("rust_{}", module_name));
        fs::create_dir_all(&temp_rust_out)?;

        // Run jtd-codegen
        let output = Command::new("jtd-codegen")
            .arg("--rust-out")
            .arg(&temp_rust_out)
            .arg("--root-name")
            .arg(&struct_name)
            .arg("--")
            .arg(&json_path)
            .output()
            .context("Failed to run jtd-codegen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jtd-codegen failed for {}: {}", yaml_path.display(), stderr);
        }

        // Read generated code and post-process
        let generated_mod = temp_rust_out.join("mod.rs");
        let generated_code = fs::read_to_string(&generated_mod).with_context(|| {
            format!("Failed to read generated code for {}", yaml_path.display())
        })?;

        let processed_code = post_process_rust(&generated_code, &struct_name)?;

        let output_path = output_dir.join(format!("{}.rs", module_name));
        fs::write(&output_path, processed_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        modules.push((module_name, struct_name));
    }

    // Generate mod.rs
    let mod_rs = generate_rust_mod_rs(&modules);
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

fn run_jtd_codegen_python(schema_files: &[PathBuf], output_dir: &Path) -> Result<()> {
    verify_jtd_codegen()?;

    if output_dir.exists() {
        fs::remove_dir_all(output_dir).context("Failed to clean output directory")?;
    }
    fs::create_dir_all(output_dir).context("Failed to create output directory")?;

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let mut modules: Vec<(String, String)> = Vec::new();

    for yaml_path in schema_files {
        println!("  Processing: {}", yaml_path.display());

        let (module_name, class_name, json_path) = prepare_schema(yaml_path, temp_dir.path())?;

        // Create temp output dir
        let temp_python_out = temp_dir.path().join(format!("python_{}", module_name));
        fs::create_dir_all(&temp_python_out)?;

        // Run jtd-codegen --python-out
        let output = Command::new("jtd-codegen")
            .arg("--python-out")
            .arg(&temp_python_out)
            .arg("--root-name")
            .arg(&class_name)
            .arg("--")
            .arg(&json_path)
            .output()
            .context("Failed to run jtd-codegen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jtd-codegen failed for {}: {}", yaml_path.display(), stderr);
        }

        // Read generated Python file
        let generated_init = temp_python_out.join("__init__.py");
        let python_code = fs::read_to_string(&generated_init).with_context(|| {
            format!(
                "Failed to read generated Python for {}",
                yaml_path.display()
            )
        })?;

        // Post-process: add copyright header
        let processed_code = post_process_python(&python_code);

        let output_path = output_dir.join(format!("{}.py", module_name));
        fs::write(&output_path, &processed_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        println!("    -> {}.py (class {})", module_name, class_name);
        modules.push((module_name, class_name));
    }

    // Generate __init__.py
    let init_py = generate_python_init_py(&modules);
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
// TypeScript codegen
// =============================================================================

fn run_jtd_codegen_typescript(schema_files: &[PathBuf], output_dir: &Path) -> Result<()> {
    verify_jtd_codegen()?;

    if output_dir.exists() {
        fs::remove_dir_all(output_dir).context("Failed to clean output directory")?;
    }
    fs::create_dir_all(output_dir).context("Failed to create output directory")?;

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let mut modules: Vec<(String, String)> = Vec::new();

    for yaml_path in schema_files {
        println!("  Processing: {}", yaml_path.display());

        let (module_name, class_name, json_path) = prepare_schema(yaml_path, temp_dir.path())?;

        // Create temp output dir
        let temp_ts_out = temp_dir.path().join(format!("ts_{}", module_name));
        fs::create_dir_all(&temp_ts_out)?;

        // Run jtd-codegen --typescript-out
        let output = Command::new("jtd-codegen")
            .arg("--typescript-out")
            .arg(&temp_ts_out)
            .arg("--root-name")
            .arg(&class_name)
            .arg("--")
            .arg(&json_path)
            .output()
            .context("Failed to run jtd-codegen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jtd-codegen failed for {}: {}", yaml_path.display(), stderr);
        }

        // Read generated TypeScript file
        let generated_index = temp_ts_out.join("index.ts");
        let ts_code = fs::read_to_string(&generated_index).with_context(|| {
            format!(
                "Failed to read generated TypeScript for {}",
                yaml_path.display()
            )
        })?;

        // Post-process: add copyright header
        let processed_code = post_process_typescript(&ts_code);

        let output_path = output_dir.join(format!("{}.ts", module_name));
        fs::write(&output_path, &processed_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        println!("    -> {}.ts (interface {})", module_name, class_name);
        modules.push((module_name, class_name));
    }

    // Generate index.ts
    let index_ts = generate_typescript_index_ts(&modules);
    let index_path = output_dir.join("index.ts");
    fs::write(&index_path, &index_ts).context("Failed to write index.ts")?;

    println!(
        "Generated {} TypeScript modules in {}",
        modules.len(),
        output_dir.display()
    );

    Ok(())
}

// =============================================================================
// Post-processing
// =============================================================================

/// Post-process jtd-codegen Rust output.
///
/// Transforms jtd-codegen v0.4.1 output to match the project's expected format:
/// - Adds copyright header and standard derives
/// - Converts camelCase field names to snake_case
/// - Removes Box<> wrappers from optional fields
/// - Removes skip_serializing_if annotations
/// - Adds #[serde(deny_unknown_fields)] to structs
/// - Strips struct name prefix from enum names
/// - Adds #[default] to first enum variant
fn post_process_rust(code: &str, expected_struct_name: &str) -> Result<String> {
    // First pass: collect struct names.
    // jtd-codegen puts sub-structs first, root struct last.
    let mut struct_names: Vec<String> = Vec::new();
    for line in code.lines() {
        if line.starts_with("pub struct ") {
            let name = line
                .trim_start_matches("pub struct ")
                .split([' ', '{'])
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                struct_names.push(name.to_string());
            }
        }
    }

    // The root struct is the last one in jtd-codegen output.
    // Only fix the root struct name if it differs from expected.
    let root_struct_name = struct_names.last().cloned();
    let code = if let Some(ref actual_root) = root_struct_name {
        if actual_root != expected_struct_name {
            // Only replace exact matches (the root name, not sub-struct names that start with it)
            // Since sub-structs are named like RootNameSubfield, we need word-boundary matching.
            // Replace "actual_root" only when NOT followed by more PascalCase chars.
            let mut result = String::new();
            for line in code.lines() {
                let new_line = replace_exact_type_name(line, actual_root, expected_struct_name);
                result.push_str(&new_line);
                result.push('\n');
            }
            result
        } else {
            code.to_string()
        }
    } else {
        code.to_string()
    };
    let code = code.as_str();

    // Update struct_names to reflect the fixup
    let struct_names: Vec<String> = struct_names
        .into_iter()
        .map(|n| {
            if let Some(ref actual_root) = root_struct_name {
                if n == *actual_root && actual_root.as_str() != expected_struct_name {
                    return expected_struct_name.to_string();
                }
            }
            n
        })
        .collect();

    let mut result = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\n\
         //! Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n",
    );

    let mut in_enum = false;
    let mut in_struct = false;
    let mut marked_first_variant = false;
    let mut next_is_struct = false;
    // Map from prefixed enum name to short name
    let mut enum_renames: Vec<(String, String)> = Vec::new();

    for line in code.lines() {
        // Skip the jtd-codegen version comment and chrono import
        if line.starts_with("// Code generated by jtd-codegen") {
            continue;
        }
        if line.starts_with("use chrono::") {
            continue;
        }

        // Enhance derives
        if line.contains("#[derive(Serialize, Deserialize)]")
            || line.contains("#[derive(Debug, Serialize, Deserialize)]")
        {
            result
                .push_str("#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]\n");
            next_is_struct = true;
            continue;
        }

        // Skip skip_serializing_if annotations
        if line.trim().starts_with("#[serde(skip_serializing_if") {
            continue;
        }

        // Track struct boundaries — strip root name prefix from sub-struct names
        if next_is_struct && line.starts_with("pub struct ") {
            let full_name = line
                .trim_start_matches("pub struct ")
                .split([' ', '{'])
                .next()
                .unwrap_or("");

            // Try to strip root struct name prefix from sub-structs
            let mut short_name = full_name.to_string();
            // Only rename if this is NOT the root struct (i.e., the expected name)
            if full_name != expected_struct_name
                && full_name.starts_with(expected_struct_name)
                && full_name.len() > expected_struct_name.len()
            {
                short_name = full_name[expected_struct_name.len()..].to_string();
                enum_renames.push((full_name.to_string(), short_name.clone()));
            }

            result.push_str("#[serde(deny_unknown_fields)]\n");
            result.push_str(&format!("pub struct {} {{\n", short_name));
            in_struct = true;
            next_is_struct = false;
            continue;
        }

        // Track enum start — strip struct name prefix from enum name
        if next_is_struct && line.starts_with("pub enum ") {
            in_enum = true;
            in_struct = false;
            marked_first_variant = false;
            next_is_struct = false;

            let full_name = line
                .trim_start_matches("pub enum ")
                .split([' ', '{'])
                .next()
                .unwrap_or("");

            // Try to strip struct name prefix
            let mut short_name = full_name.to_string();
            for struct_name in &struct_names {
                if full_name.starts_with(struct_name.as_str())
                    && full_name.len() > struct_name.len()
                {
                    short_name = full_name[struct_name.len()..].to_string();
                    enum_renames.push((full_name.to_string(), short_name.clone()));
                    break;
                }
            }

            result.push_str(&format!("pub enum {} {{\n", short_name));
            continue;
        }

        next_is_struct = false;

        // Add #[default] to first enum variant
        if in_enum && !marked_first_variant {
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !trimmed.starts_with("//")
                && !trimmed.starts_with('}')
            {
                result.push_str("    #[default]\n");
                marked_first_variant = true;
            }
        }

        // End of enum/struct
        if line.trim() == "}" {
            if in_enum {
                in_enum = false;
            }
            if in_struct {
                in_struct = false;
            }
        }

        // Process struct fields: camelCase → snake_case, remove Box<>
        if in_struct
            && line.starts_with("    pub ")
            && line.contains(": ")
            && !line.trim().starts_with('#')
            && !line.trim().starts_with("//")
        {
            let mut processed_line = line.to_string();

            // Convert camelCase field name to snake_case
            // Field lines look like: "    pub fieldName: Type,"
            if let Some(field_start) = processed_line.find("pub ") {
                let after_pub = &processed_line[field_start + 4..];
                if let Some(colon_pos) = after_pub.find(':') {
                    let field_name = &after_pub[..colon_pos];
                    let snake_name = camel_to_snake(field_name);
                    if snake_name != field_name {
                        processed_line = format!(
                            "{}pub {}{}",
                            &processed_line[..field_start],
                            snake_name,
                            &after_pub[colon_pos..]
                        );
                    }
                }
            }

            // Remove Box<> wrappers: Option<Box<T>> → Option<T>
            processed_line = processed_line.replace("Option<Box<", "Option<");
            if processed_line.contains("Option<") {
                // Remove the matching closing >
                // "Option<String>>" → "Option<String>"
                processed_line = processed_line.replacen(">>", ">", 1);
            }

            // Replace prefixed enum type names with short names in field types
            for (full_name, short_name) in &enum_renames {
                processed_line = processed_line.replace(full_name.as_str(), short_name.as_str());
            }

            result.push_str(&processed_line);
            result.push('\n');
            continue;
        }

        // For non-field lines, also replace prefixed enum names
        let mut processed_line = line.to_string();
        for (full_name, short_name) in &enum_renames {
            processed_line = processed_line.replace(full_name.as_str(), short_name.as_str());
        }

        result.push_str(&processed_line);
        result.push('\n');
    }

    Ok(result)
}

/// Replace a type name in a line only when it appears as an exact match
/// (not as a prefix of a longer PascalCase name).
fn replace_exact_type_name(line: &str, old_name: &str, new_name: &str) -> String {
    let mut result = String::new();
    let mut remaining = line;

    while let Some(pos) = remaining.find(old_name) {
        result.push_str(&remaining[..pos]);

        let after = &remaining[pos + old_name.len()..];

        // Check if the match is followed by an uppercase letter (part of a longer name)
        let next_char = after.chars().next();
        if next_char.map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
            // This is a prefix of a longer name — don't replace
            result.push_str(old_name);
        } else {
            result.push_str(new_name);
        }

        remaining = after;
    }

    result.push_str(remaining);
    result
}

/// Convert camelCase to snake_case.
fn camel_to_snake(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Post-process jtd-codegen Python output.
fn post_process_python(code: &str) -> String {
    format!(
        "# Copyright (c) 2025 Jonathan Fontanez\n\
         # SPDX-License-Identifier: BUSL-1.1\n\
         #\n\
         # Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n{}",
        code
    )
}

/// Post-process jtd-codegen TypeScript output.
fn post_process_typescript(code: &str) -> String {
    format!(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\
         //\n\
         // Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n{}",
        code
    )
}

// =============================================================================
// Barrel file generation
// =============================================================================

/// Generate mod.rs for Rust.
fn generate_rust_mod_rs(modules: &[(String, String)]) -> String {
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

/// Generate __init__.py for Python.
fn generate_python_init_py(modules: &[(String, String)]) -> String {
    let mut init_py = String::from(
        "# Copyright (c) 2025 Jonathan Fontanez\n\
         # SPDX-License-Identifier: BUSL-1.1\n\
         #\n\
         # Generated by jtd-codegen. DO NOT EDIT.\n\n",
    );

    for (module_name, class_name) in modules {
        init_py.push_str(&format!("from .{} import {}\n", module_name, class_name));
    }

    init_py.push_str("\n__all__ = [\n");
    for (_, class_name) in modules {
        init_py.push_str(&format!("    \"{}\",\n", class_name));
    }
    init_py.push_str("]\n");

    init_py
}

/// Generate index.ts for TypeScript.
fn generate_typescript_index_ts(modules: &[(String, String)]) -> String {
    let mut index_ts = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\
         //\n\
         // Generated by jtd-codegen. DO NOT EDIT.\n\n",
    );

    for (module_name, _) in modules {
        index_ts.push_str(&format!("export * from \"./{}.ts\";\n", module_name));
    }

    index_ts
}

// =============================================================================
// Name conversion helpers
// =============================================================================

/// Convert schema name to struct name (PascalCase).
fn schema_name_to_struct_name(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
    let last_segment = name.split('.').next_back().unwrap_or(name);

    // Handle special case for "config" suffix
    if last_segment == "config" {
        let segments: Vec<&str> = name.split('.').collect();
        if segments.len() >= 2 {
            let processor_name = segments[segments.len() - 2];
            return format!("{}Config", to_pascal_case(processor_name));
        }
    }

    // Handle channel suffixes like "1ch", "2ch"
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
fn schema_name_to_module_name(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
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
