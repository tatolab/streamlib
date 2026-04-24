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

        // Post-process: add copyright header and enforce expected class name
        let processed_code = post_process_python(&python_code, &class_name);

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
/// - Preserves skip_serializing_if annotations (correct wire behavior)
/// - Adds #[serde(deny_unknown_fields)] to structs
/// - Strips struct/enum name prefix from sub-type names in plain schemas
/// - Adds #[default] to first variant of plain enums
/// - Handles discriminator (tagged) enums: no Default derive, no #[default],
///   preserves #[serde(tag = "…")] ordering, keeps variant payload structs
///   as fully-qualified names (avoids Ok/Err collisions with std::result).
fn post_process_rust(code: &str, expected_struct_name: &str) -> Result<String> {
    let lines: Vec<&str> = code.lines().collect();

    // Collect all `pub struct` / `pub enum` names. jtd-codegen puts sub-types
    // first and the root last, but we don't rely on that here.
    let struct_names: Vec<String> = lines
        .iter()
        .filter_map(|line| extract_decl_name(line, "pub struct "))
        .collect();
    let enum_names: Vec<String> = lines
        .iter()
        .filter_map(|line| extract_decl_name(line, "pub enum "))
        .collect();

    // Detect discriminator (tagged) enum: `#[serde(tag = "…")]` immediately
    // preceding a `pub enum Name`. The enum is the root of the schema, not
    // any of the per-variant structs.
    let discriminator_enum_name = find_discriminator_enum_name(&lines);
    let is_discriminator = discriminator_enum_name.is_some();

    // Root-rename pass: for plain struct schemas, jtd-codegen's last `pub
    // struct` is the root and may be named something other than
    // `expected_struct_name` (e.g. `Whep` vs `WebrtcWhepConfig`). Rewrite the
    // text so the root name matches. Skip for discriminator schemas — their
    // root is the enum, which already carries `expected_struct_name`.
    let rewritten;
    let code: &str = if !is_discriminator {
        let root_struct_name = struct_names.last();
        match root_struct_name {
            Some(actual_root) if actual_root != expected_struct_name => {
                let mut buf = String::new();
                for line in lines.iter() {
                    let new_line =
                        replace_exact_type_name(line, actual_root, expected_struct_name);
                    buf.push_str(&new_line);
                    buf.push('\n');
                }
                rewritten = buf;
                &rewritten
            }
            _ => code,
        }
    } else {
        code
    };

    // Precompute name renames (full → short) so lines referencing a renamed
    // type (e.g. enum variant references, field types) can be rewritten in a
    // single pass. For discriminator schemas we leave variant payload struct
    // names as-is (avoids `Ok`/`Err` collisions with std::result and keeps
    // Rust output in step with Python/TS which also keep full names).
    let post_lines: Vec<&str> = code.lines().collect();
    let mut name_renames: Vec<(String, String)> = Vec::new();
    let discriminator_enum_ref = discriminator_enum_name.as_deref();
    if !is_discriminator {
        for name in &struct_names {
            let short = strip_prefix(name, expected_struct_name);
            if short != *name {
                name_renames.push((name.clone(), short));
            }
        }
        for name in &enum_names {
            for struct_name in &struct_names {
                if name.starts_with(struct_name.as_str()) && name.len() > struct_name.len() {
                    let short = name[struct_name.len()..].to_string();
                    name_renames.push((name.clone(), short));
                    break;
                }
            }
        }
    }
    // Sort renames by descending length of the full name so longer prefixes
    // match first (prevents `Foo` rewriting the start of `FooBar`).
    name_renames.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut result = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\n\
         //! Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n",
    );

    let mut in_enum = false;
    let mut in_struct = false;
    let mut in_discriminator_enum = false;
    let mut marked_first_variant = false;
    let mut pending_decl_kind: Option<DeclKind> = None;
    let mut pending_attr_lines: Vec<String> = Vec::new();

    for (idx, line) in post_lines.iter().enumerate() {
        // Skip the jtd-codegen version comment and chrono import.
        if line.starts_with("// Code generated by jtd-codegen") {
            continue;
        }
        if line.starts_with("use chrono::") {
            continue;
        }

        // Defer derive emission until we see the upcoming declaration — the
        // derive bundle depends on declaration kind (struct / plain enum /
        // discriminator enum).
        if line.contains("#[derive(Serialize, Deserialize)]")
            || line.contains("#[derive(Debug, Serialize, Deserialize)]")
        {
            pending_decl_kind =
                Some(peek_decl_kind(&post_lines, idx, discriminator_enum_ref));
            pending_attr_lines.clear();
            continue;
        }

        // Buffer attribute lines that sit between a deferred `#[derive(...)]`
        // and the upcoming `pub struct`/`pub enum` (e.g. `#[serde(tag = "…")]`).
        if pending_decl_kind.is_some()
            && !line.starts_with("pub struct ")
            && !line.starts_with("pub enum ")
            && line.trim().starts_with("#[")
        {
            pending_attr_lines.push(line.to_string());
            continue;
        }

        // Emit a struct declaration.
        if pending_decl_kind == Some(DeclKind::Struct) && line.starts_with("pub struct ") {
            let full_name = extract_decl_name(line, "pub struct ").unwrap_or_default();
            let short_name = rename_lookup(&name_renames, &full_name).unwrap_or(&full_name);

            result.push_str(
                "#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]\n",
            );
            for attr in pending_attr_lines.drain(..) {
                result.push_str(&attr);
                result.push('\n');
            }
            result.push_str("#[serde(deny_unknown_fields)]\n");
            result.push_str(&format!("pub struct {} {{\n", short_name));
            in_struct = true;
            in_enum = false;
            in_discriminator_enum = false;
            pending_decl_kind = None;
            continue;
        }

        // Emit an enum declaration.
        if matches!(
            pending_decl_kind,
            Some(DeclKind::RegularEnum) | Some(DeclKind::DiscriminatorEnum)
        ) && line.starts_with("pub enum ")
        {
            let full_name = extract_decl_name(line, "pub enum ").unwrap_or_default();
            let short_name = rename_lookup(&name_renames, &full_name).unwrap_or(&full_name);
            let is_disc_variant = pending_decl_kind == Some(DeclKind::DiscriminatorEnum);

            if is_disc_variant {
                // Wire enum: explicit construction only, no Default.
                result.push_str(
                    "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n",
                );
            } else {
                result.push_str(
                    "#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]\n",
                );
            }
            for attr in pending_attr_lines.drain(..) {
                result.push_str(&attr);
                result.push('\n');
            }
            result.push_str(&format!("pub enum {} {{\n", short_name));
            in_enum = true;
            in_struct = false;
            in_discriminator_enum = is_disc_variant;
            marked_first_variant = false;
            pending_decl_kind = None;
            continue;
        }

        // Plain enums only: insert `#[default]` on the first real variant to
        // satisfy the Default derive. Discriminator enums don't derive
        // Default.
        if in_enum && !in_discriminator_enum && !marked_first_variant {
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

        // End of enum/struct.
        if line.trim() == "}" {
            if in_enum {
                in_enum = false;
                in_discriminator_enum = false;
            }
            if in_struct {
                in_struct = false;
            }
        }

        // Struct field: camelCase → snake_case, remove Box<>.
        if in_struct
            && line.starts_with("    pub ")
            && line.contains(": ")
            && !line.trim().starts_with('#')
            && !line.trim().starts_with("//")
        {
            let mut processed_line = line.to_string();

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

            // Strip the `Box<...>` that jtd-codegen wraps optional recursive
            // field types in, but only touch `>>` that we actually opened —
            // otherwise unrelated nested generics like
            // `HashMap<String, Option<Value>>` lose their closing bracket.
            let boxed_count = processed_line.matches("Option<Box<").count();
            if boxed_count > 0 {
                processed_line = processed_line.replace("Option<Box<", "Option<");
                for _ in 0..boxed_count {
                    processed_line = processed_line.replacen(">>", ">", 1);
                }
            }

            for (full_name, short_name) in &name_renames {
                processed_line = processed_line.replace(full_name.as_str(), short_name.as_str());
            }

            result.push_str(&processed_line);
            result.push('\n');
            continue;
        }

        // Non-field lines (doc comments, enum variants, closing braces) —
        // apply name renames so variant payload types reference the stripped
        // type names.
        let mut processed_line = line.to_string();
        for (full_name, short_name) in &name_renames {
            processed_line = processed_line.replace(full_name.as_str(), short_name.as_str());
        }

        result.push_str(&processed_line);
        result.push('\n');
    }

    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    Struct,
    RegularEnum,
    DiscriminatorEnum,
}

/// Look up the short form of a type name in the precomputed rename list.
fn rename_lookup<'a>(
    renames: &'a [(String, String)],
    full_name: &str,
) -> Option<&'a String> {
    renames
        .iter()
        .find(|(full, _)| full == full_name)
        .map(|(_, short)| short)
}

/// Peek forward from a `#[derive(...)]` line to determine what declaration
/// kind follows. Stops at the first `pub struct` / `pub enum`.
fn peek_decl_kind(
    lines: &[&str],
    derive_idx: usize,
    discriminator_enum_name: Option<&str>,
) -> DeclKind {
    for line in lines.iter().skip(derive_idx + 1) {
        if line.starts_with("pub struct ") {
            return DeclKind::Struct;
        }
        if line.starts_with("pub enum ") {
            let name = extract_decl_name(line, "pub enum ").unwrap_or_default();
            return if discriminator_enum_name == Some(name.as_str()) {
                DeclKind::DiscriminatorEnum
            } else {
                DeclKind::RegularEnum
            };
        }
    }
    DeclKind::Struct
}

/// Find the first `pub enum` whose immediately-preceding attributes include
/// `#[serde(tag = "…")]`. That enum is a discriminator (tagged) enum and is
/// the schema root for JTD discriminator form.
fn find_discriminator_enum_name(lines: &[&str]) -> Option<String> {
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with("pub enum ") {
            continue;
        }
        // Walk backward through the contiguous run of attribute lines
        // preceding this enum declaration.
        let mut j = i;
        while j > 0 {
            j -= 1;
            let trimmed = lines[j].trim();
            if trimmed.is_empty() || !trimmed.starts_with("#[") {
                break;
            }
            if extract_tag_attr(trimmed).is_some() {
                return extract_decl_name(line, "pub enum ");
            }
        }
    }
    None
}

/// Extract the value of `tag = "…"` from a `#[serde(...)]` attribute line.
fn extract_tag_attr(line: &str) -> Option<&str> {
    if !line.starts_with("#[serde(") {
        return None;
    }
    let after = line.split("tag = \"").nth(1)?;
    let close_quote = after.find('"')?;
    Some(&after[..close_quote])
}

/// Extract the name from a `pub struct Foo` / `pub enum Foo` declaration line.
fn extract_decl_name(line: &str, prefix: &str) -> Option<String> {
    let rest = line.strip_prefix(prefix)?;
    let name = rest.split([' ', '{']).next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Strip `prefix` from `name` if `name` starts with it and is strictly longer.
/// Returns `name` unchanged if the prefix doesn't apply.
fn strip_prefix(name: &str, prefix: &str) -> String {
    if name != prefix && name.starts_with(prefix) && name.len() > prefix.len() {
        name[prefix.len()..].to_string()
    } else {
        name.to_string()
    }
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
///
/// jtd-codegen's Python backend ignores `--root-name` and runs its own
/// acronym-upcasing pass on the schema name (`api_server` → `APIServerConfig`,
/// `http_config` → `HTTPConfig`, etc.), so the root class name drifts from
/// what the generator config expects and from the Rust/TypeScript outputs.
/// Rewrite the root class name so every language ends up with the same
/// symbol.
fn post_process_python(code: &str, expected_class_name: &str) -> String {
    let actual = find_python_root_class_name(code);
    let rewritten = match actual {
        Some(ref name) if name != expected_class_name => {
            code.replace(name.as_str(), expected_class_name)
        }
        _ => code.to_string(),
    };

    format!(
        "# Copyright (c) 2025 Jonathan Fontanez\n\
         # SPDX-License-Identifier: BUSL-1.1\n\
         #\n\
         # Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n{}",
        rewritten
    )
}

/// Return the name of the root class in a jtd-codegen Python file.
///
/// For discriminator schemas, jtd-codegen emits the parent class first and
/// variants that inherit from it afterwards (`class Variant(Root):`) — the
/// root is whichever local class is referenced as a parent of another.
///
/// For plain schemas with sub-types, jtd-codegen emits the sub-types first
/// and the root last, so the last top-level class is the root.
fn find_python_root_class_name(code: &str) -> Option<String> {
    let mut classes: Vec<(String, Option<String>)> = Vec::new();
    for line in code.lines() {
        let Some(rest) = line.strip_prefix("class ") else {
            continue;
        };
        let name_end = rest.find(|c: char| c == '(' || c == ':' || c == ' ');
        let (name, parent) = match name_end {
            Some(idx) if rest.as_bytes().get(idx) == Some(&b'(') => {
                let name = rest[..idx].to_string();
                let after = &rest[idx + 1..];
                let parent = after
                    .find(')')
                    .map(|end| after[..end].trim().to_string())
                    .filter(|p| !p.is_empty());
                (name, parent)
            }
            Some(idx) => (rest[..idx].to_string(), None),
            None => (rest.to_string(), None),
        };
        if !name.is_empty() {
            classes.push((name, parent));
        }
    }

    let local_names: std::collections::HashSet<&str> =
        classes.iter().map(|(n, _)| n.as_str()).collect();
    for (_, parent) in &classes {
        if let Some(p) = parent {
            if local_names.contains(p.as_str()) {
                return Some(p.clone());
            }
        }
    }
    classes.last().map(|(n, _)| n.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_python_root_class_name_skips_imports() {
        let code = "import re\nfrom dataclasses import dataclass\n\n\n@dataclass\nclass APIServerConfig:\n    host: 'str'\n";
        assert_eq!(
            find_python_root_class_name(code).as_deref(),
            Some("APIServerConfig")
        );
    }

    #[test]
    fn find_python_root_class_name_picks_last_for_plain_multiclass() {
        // jtd-codegen emits sub-types first and the root last for plain
        // schemas that have nested variant enums (e.g. WebrtcWhepConfig).
        let code = "class WebrtcWhepConfigWhep:\n    pass\nclass WebrtcWhepConfig:\n    pass\n";
        assert_eq!(
            find_python_root_class_name(code).as_deref(),
            Some("WebrtcWhepConfig")
        );
    }

    #[test]
    fn find_python_root_class_name_picks_parent_for_discriminator() {
        // For discriminator schemas the root is the parent class that
        // variants inherit from.
        let code = "class EscalateRequest:\n    pass\nclass EscalateRequestAcquirePixelBuffer(EscalateRequest):\n    pass\n";
        assert_eq!(
            find_python_root_class_name(code).as_deref(),
            Some("EscalateRequest")
        );
    }

    #[test]
    fn find_python_root_class_name_none_when_no_class() {
        assert_eq!(find_python_root_class_name("import re\n"), None);
    }

    #[test]
    fn post_process_python_renames_upcased_acronym() {
        let code = "@dataclass\nclass APIServerConfig:\n    @classmethod\n    def from_json_data(cls, data) -> 'APIServerConfig':\n        return cls()\n";
        let out = post_process_python(code, "ApiServerConfig");
        assert!(out.contains("class ApiServerConfig:"));
        assert!(out.contains("-> 'ApiServerConfig':"));
        assert!(!out.contains("APIServerConfig"));
    }

    #[test]
    fn post_process_python_noop_when_names_match() {
        let code = "class WebrtcWhepConfig:\n    pass\n";
        let out = post_process_python(code, "WebrtcWhepConfig");
        // Header added, body unchanged
        assert!(out.contains("class WebrtcWhepConfig:"));
        assert_eq!(out.matches("WebrtcWhepConfig").count(), 1);
    }
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
