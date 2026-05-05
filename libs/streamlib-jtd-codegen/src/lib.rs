// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JTD-codegen pipeline: schema YAML files → typed Rust/Python/TypeScript bindings.
//!
//! Three-pass shape (Decision 7 of milestone-10's
//! `docs/architecture/schema-identity-and-packaging.md`):
//!
//! 1. **Resolve** — read `streamlib.yaml` + `streamlib.lock`, walk the
//!    dependency graph, produce `(SchemaIdent, JtdSchema)` pairs.
//! 2. **Substitute → generate → substitute back** — replace cross-package
//!    refs with deterministic sentinels, run `jtd-codegen`, restore native
//!    cross-package imports. Implementation: [`sentinel`].
//! 3. **Order** — stable-sort properties by name so the output is
//!    diff-stable. Implementation: [`ordering`].
//!
//! Public entry points:
//!
//! - [`generate`] — driver for the CLI / xtask, accepting `--project-dir`,
//!   `--schema-file`, or `--schema-dir`.
//! - [`generate_from_resolved`] — direct entry for code that's already run
//!   the resolver (build scripts, integration tests).

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_idents::{ResolvedPackages, ResolverOptions};

pub mod ordering;
pub mod sentinel;

pub use sentinel::SentinelTable;

/// Root-name sentinel passed to `jtd-codegen --root-name`. The post-processor
/// substitutes this back to the per-schema identifier from `streamlib.yaml`.
///
/// Sidesteps three known `jtd-codegen` v0.4.1 bugs uniformly across all three
/// backends: digit-boundary lowercasing (`H264D` → `H264d`), Python-only
/// acronym upcasing (`Api` → `API`), and inconsistent `--root-name` mangling
/// shape per emit case (struct vs. discriminator vs. type alias). The sentinel
/// is shaped so all three backends preserve it byte-identically — verified
/// empirically; underscore-prefixed sentinels (e.g. `__ROOT__`) collapse to
/// `Root` because jtd-codegen normalizes identifiers per its own rules.
///
/// Per-schema identifier sources are the schema's `metadata.name` field via
/// [`schema_name_to_struct_name`]; the sentinel is a transport detail of the
/// codegen pipeline and never appears in committed `_generated_/` output.
const ROOT_NAME_SENTINEL: &str = "StreamlibCanonRoot";

/// Target runtime language for schema code generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RuntimeTarget {
    Rust,
    Python,
    Typescript,
}

/// Options for [`generate`].
pub struct GenerateOptions {
    /// Target runtime language.
    pub runtime: RuntimeTarget,
    /// Output directory for generated bindings.
    pub output: PathBuf,
    /// `streamlib.yaml`-driven mode: directory containing the project
    /// manifest. The resolver walks declared dependencies and the codegen
    /// pipeline ingests the resulting `(SchemaIdent, JtdSchema)` set.
    pub project_dir: Option<PathBuf>,
    /// Single-schema mode (kept for ad-hoc use).
    pub schema_file: Option<PathBuf>,
    /// Directory-of-yaml mode (kept for ad-hoc use).
    pub schema_dir: Option<PathBuf>,
    /// Workspace root used to resolve project-relative paths in CLI args.
    pub workspace_root: PathBuf,
    /// When `project_dir` mode is used and dependencies were declared,
    /// write `streamlib.lock` next to `streamlib.yaml`. Defaults to `true`.
    pub write_lockfile: bool,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            runtime: RuntimeTarget::Rust,
            output: PathBuf::new(),
            project_dir: None,
            schema_file: None,
            schema_dir: None,
            workspace_root: PathBuf::new(),
            write_lockfile: true,
        }
    }
}

/// Run the JTD-codegen pipeline.
pub fn generate(opts: GenerateOptions) -> Result<()> {
    let GenerateOptions {
        runtime,
        output,
        project_dir,
        schema_file,
        schema_dir,
        workspace_root: _,
        write_lockfile,
    } = opts;

    if let Some(project_dir) = project_dir {
        let resolved = streamlib_idents::resolve_with(&project_dir, &ResolverOptions::default())
            .context("Failed to resolve streamlib.yaml dependency graph")?;

        if write_lockfile && !resolved.packages.is_empty() {
            let lockfile = resolved.to_lockfile();
            let lock_path = project_dir.join(streamlib_idents::LOCKFILE_NAME);
            streamlib_idents::write_lockfile(&lock_path, &lockfile)
                .context("Failed to write streamlib.lock")?;
            tracing::info!("Wrote {} ({} packages)", lock_path.display(), resolved.packages.len());
        }

        return generate_from_resolved(&resolved, runtime, &output);
    }

    if let Some(schema_path) = schema_file {
        if !schema_path.exists() {
            anyhow::bail!("Schema file not found: {}", schema_path.display());
        }
        return run_codegen(&[schema_path], runtime, &output);
    }

    if let Some(dir) = schema_dir {
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("Failed to read directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str());
            if matches!(ext, Some("yaml") | Some("yml")) {
                files.push(path);
            }
        }
        files.sort();
        return run_codegen(&files, runtime, &output);
    }

    anyhow::bail!("No input specified. Use --project-dir, --schema-file, or --schema-dir");
}

/// Direct entry: run codegen against an already-resolved package set.
///
/// The output layout is flat for now (one file per schema in `output`),
/// matching the in-tree `_generated_/` shape pre-#402. When carve-out
/// packages land, this layout will likely become per-package
/// (`output/<org>__<package>/<file>`); the function keeps a stable name so
/// callers don't churn.
pub fn generate_from_resolved(
    resolved: &ResolvedPackages,
    runtime: RuntimeTarget,
    output: &Path,
) -> Result<()> {
    let mut all_schema_files: Vec<PathBuf> = Vec::new();
    for pkg in resolved.iter_all() {
        all_schema_files.extend(pkg.schema_files.iter().cloned());
    }
    all_schema_files.sort();
    all_schema_files.dedup();

    if all_schema_files.is_empty() {
        tracing::info!("No schemas to generate");
        return Ok(());
    }

    run_codegen(&all_schema_files, runtime, output)
}

fn run_codegen(schema_files: &[PathBuf], runtime: RuntimeTarget, output: &Path) -> Result<()> {
    if schema_files.is_empty() {
        tracing::info!("No schemas found");
        return Ok(());
    }
    tracing::info!("Found {} schemas", schema_files.len());

    match runtime {
        RuntimeTarget::Rust => run_jtd_codegen_rust(schema_files, output),
        RuntimeTarget::Python => run_jtd_codegen_python(schema_files, output),
        RuntimeTarget::Typescript => run_jtd_codegen_typescript(schema_files, output),
    }
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

/// Convert a schema YAML file to JSON, run the sentinel pre-pass + property
/// ordering, and write the result to `temp_dir`. Returns the (module_name,
/// struct_name, json_path, sentinel_table) tuple.
fn prepare_schema(
    yaml_path: &Path,
    temp_dir: &Path,
) -> Result<(String, String, PathBuf, SentinelTable)> {
    let yaml_content = fs::read_to_string(yaml_path)
        .with_context(|| format!("Failed to read {}", yaml_path.display()))?;

    let schema: JtdSchema = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse {}", yaml_path.display()))?;

    let module_name = schema_name_to_module_name(&schema.metadata.name);
    let struct_name = schema_name_to_struct_name(&schema.metadata.name);

    // YAML → JSON value (mutable so we can run pre-passes on it).
    let mut json_value: serde_json::Value = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse YAML {}", yaml_path.display()))?;

    let mut sentinel_table = SentinelTable::default();
    sentinel::substitute(&mut json_value, &mut sentinel_table)
        .with_context(|| format!("Sentinel substitution failed for {}", yaml_path.display()))?;
    ordering::sort_object_keys_recursively(&mut json_value);

    let json_filename = format!("{}.json", struct_name);
    let json_path = temp_dir.join(&json_filename);
    let json_content =
        serde_json::to_string_pretty(&json_value).context("Failed to serialize to JSON")?;
    fs::write(&json_path, &json_content)
        .with_context(|| format!("Failed to write {}", json_path.display()))?;

    Ok((module_name, struct_name, json_path, sentinel_table))
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
        tracing::info!("  Processing: {}", yaml_path.display());

        let (module_name, struct_name, json_path, sentinel_table) =
            prepare_schema(yaml_path, temp_dir.path())?;

        let temp_rust_out = temp_dir.path().join(format!("rust_{}", module_name));
        fs::create_dir_all(&temp_rust_out)?;

        let output = Command::new("jtd-codegen")
            .arg("--rust-out")
            .arg(&temp_rust_out)
            .arg("--root-name")
            .arg(ROOT_NAME_SENTINEL)
            .arg("--")
            .arg(&json_path)
            .output()
            .context("Failed to run jtd-codegen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jtd-codegen failed for {}: {}", yaml_path.display(), stderr);
        }

        let generated_mod = temp_rust_out.join("mod.rs");
        let generated_code = fs::read_to_string(&generated_mod).with_context(|| {
            format!("Failed to read generated code for {}", yaml_path.display())
        })?;

        let processed_code = post_process_rust(&generated_code, &struct_name)?;
        let restored_code = sentinel::restore_rust(&processed_code, &sentinel_table);

        let output_path = output_dir.join(format!("{}.rs", module_name));
        fs::write(&output_path, restored_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        modules.push((module_name, struct_name));
    }

    let mod_rs = generate_rust_mod_rs(&modules);
    let mod_path = output_dir.join("mod.rs");
    fs::write(&mod_path, mod_rs).context("Failed to write mod.rs")?;

    tracing::info!(
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
        tracing::info!("  Processing: {}", yaml_path.display());

        let (module_name, class_name, json_path, sentinel_table) =
            prepare_schema(yaml_path, temp_dir.path())?;

        let temp_python_out = temp_dir.path().join(format!("python_{}", module_name));
        fs::create_dir_all(&temp_python_out)?;

        let output = Command::new("jtd-codegen")
            .arg("--python-out")
            .arg(&temp_python_out)
            .arg("--root-name")
            .arg(ROOT_NAME_SENTINEL)
            .arg("--")
            .arg(&json_path)
            .output()
            .context("Failed to run jtd-codegen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jtd-codegen failed for {}: {}", yaml_path.display(), stderr);
        }

        let generated_init = temp_python_out.join("__init__.py");
        let python_code = fs::read_to_string(&generated_init).with_context(|| {
            format!(
                "Failed to read generated Python for {}",
                yaml_path.display()
            )
        })?;

        let processed_code = post_process_python(&python_code, &class_name);
        let restored_code = sentinel::restore_python(&processed_code, &sentinel_table);

        let output_path = output_dir.join(format!("{}.py", module_name));
        fs::write(&output_path, &restored_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        tracing::info!("    -> {}.py (class {})", module_name, class_name);
        modules.push((module_name, class_name));
    }

    let init_py = generate_python_init_py(&modules);
    let init_path = output_dir.join("__init__.py");
    fs::write(&init_path, &init_py).context("Failed to write __init__.py")?;

    tracing::info!(
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
        tracing::info!("  Processing: {}", yaml_path.display());

        let (module_name, class_name, json_path, sentinel_table) =
            prepare_schema(yaml_path, temp_dir.path())?;

        let temp_ts_out = temp_dir.path().join(format!("ts_{}", module_name));
        fs::create_dir_all(&temp_ts_out)?;

        let output = Command::new("jtd-codegen")
            .arg("--typescript-out")
            .arg(&temp_ts_out)
            .arg("--root-name")
            .arg(ROOT_NAME_SENTINEL)
            .arg("--")
            .arg(&json_path)
            .output()
            .context("Failed to run jtd-codegen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jtd-codegen failed for {}: {}", yaml_path.display(), stderr);
        }

        let generated_index = temp_ts_out.join("index.ts");
        let ts_code = fs::read_to_string(&generated_index).with_context(|| {
            format!(
                "Failed to read generated TypeScript for {}",
                yaml_path.display()
            )
        })?;

        let processed_code = post_process_typescript(&ts_code, &class_name);
        let restored_code = sentinel::restore_typescript(&processed_code, &sentinel_table);

        let output_path = output_dir.join(format!("{}.ts", module_name));
        fs::write(&output_path, &restored_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        tracing::info!("    -> {}.ts (interface {})", module_name, class_name);
        modules.push((module_name, class_name));
    }

    let index_ts = generate_typescript_index_ts(&modules);
    let index_path = output_dir.join("index.ts");
    fs::write(&index_path, &index_ts).context("Failed to write index.ts")?;

    tracing::info!(
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

    let struct_names: Vec<String> = lines
        .iter()
        .filter_map(|line| extract_decl_name(line, "pub struct "))
        .collect();
    let enum_names: Vec<String> = lines
        .iter()
        .filter_map(|line| extract_decl_name(line, "pub enum "))
        .collect();

    let discriminator_enum_name = find_discriminator_enum_name(&lines);
    let is_discriminator = discriminator_enum_name.is_some();

    let post_lines = &lines;
    let mut name_renames: Vec<(String, String)> = Vec::new();
    let discriminator_enum_ref = discriminator_enum_name.as_deref();
    if !is_discriminator {
        for name in &struct_names {
            let short = strip_prefix(name, ROOT_NAME_SENTINEL);
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
        if line.starts_with("// Code generated by jtd-codegen") {
            continue;
        }
        if line.starts_with("use chrono::") {
            continue;
        }

        if line.contains("#[derive(Serialize, Deserialize)]")
            || line.contains("#[derive(Debug, Serialize, Deserialize)]")
        {
            pending_decl_kind =
                Some(peek_decl_kind(&post_lines, idx, discriminator_enum_ref));
            pending_attr_lines.clear();
            continue;
        }

        if pending_decl_kind.is_some()
            && !line.starts_with("pub struct ")
            && !line.starts_with("pub enum ")
            && line.trim().starts_with("#[")
        {
            pending_attr_lines.push(line.to_string());
            continue;
        }

        if pending_decl_kind == Some(DeclKind::Struct) && line.starts_with("pub struct ") {
            let full_name = extract_decl_name(line, "pub struct ").unwrap_or_default();
            let short_name = rename_lookup(&name_renames, &full_name).unwrap_or(&full_name);

            // Empty-struct-on-one-line case: jtd-codegen emits `pub struct X {}`
            // when JTD declares `optionalProperties: {}` (decoder configs that
            // take no knobs today). Treat it as a complete decl so the trailing
            // `}` doesn't get dropped.
            let is_empty_inline = line.trim_end().ends_with("{}");

            result.push_str(
                "#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]\n",
            );
            for attr in pending_attr_lines.drain(..) {
                result.push_str(&attr);
                result.push('\n');
            }
            result.push_str("#[serde(deny_unknown_fields)]\n");
            if is_empty_inline {
                result.push_str(&format!("pub struct {} {{}}\n", short_name));
                in_struct = false;
            } else {
                result.push_str(&format!("pub struct {} {{\n", short_name));
                in_struct = true;
            }
            in_enum = false;
            in_discriminator_enum = false;
            pending_decl_kind = None;
            continue;
        }

        if matches!(
            pending_decl_kind,
            Some(DeclKind::RegularEnum) | Some(DeclKind::DiscriminatorEnum)
        ) && line.starts_with("pub enum ")
        {
            let full_name = extract_decl_name(line, "pub enum ").unwrap_or_default();
            let short_name = rename_lookup(&name_renames, &full_name).unwrap_or(&full_name);
            let is_disc_variant = pending_decl_kind == Some(DeclKind::DiscriminatorEnum);

            if is_disc_variant {
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

        if line.trim() == "}" {
            if in_enum {
                in_enum = false;
                in_discriminator_enum = false;
            }
            if in_struct {
                in_struct = false;
            }
        }

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

        let mut processed_line = line.to_string();
        for (full_name, short_name) in &name_renames {
            processed_line = processed_line.replace(full_name.as_str(), short_name.as_str());
        }

        result.push_str(&processed_line);
        result.push('\n');
    }

    Ok(result.replace(ROOT_NAME_SENTINEL, expected_struct_name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    Struct,
    RegularEnum,
    DiscriminatorEnum,
}

fn rename_lookup<'a>(
    renames: &'a [(String, String)],
    full_name: &str,
) -> Option<&'a String> {
    renames
        .iter()
        .find(|(full, _)| full == full_name)
        .map(|(_, short)| short)
}

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

fn find_discriminator_enum_name(lines: &[&str]) -> Option<String> {
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with("pub enum ") {
            continue;
        }
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

fn extract_tag_attr(line: &str) -> Option<&str> {
    if !line.starts_with("#[serde(") {
        return None;
    }
    let after = line.split("tag = \"").nth(1)?;
    let close_quote = after.find('"')?;
    Some(&after[..close_quote])
}

fn extract_decl_name(line: &str, prefix: &str) -> Option<String> {
    let rest = line.strip_prefix(prefix)?;
    let name = rest.split([' ', '{']).next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn strip_prefix(name: &str, prefix: &str) -> String {
    if name != prefix && name.starts_with(prefix) && name.len() > prefix.len() {
        name[prefix.len()..].to_string()
    } else {
        name.to_string()
    }
}

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
fn post_process_python(code: &str, expected_class_name: &str) -> String {
    let rewritten = code.replace(ROOT_NAME_SENTINEL, expected_class_name);

    format!(
        "# Copyright (c) 2025 Jonathan Fontanez\n\
         # SPDX-License-Identifier: BUSL-1.1\n\
         #\n\
         # Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n{}",
        rewritten
    )
}

/// Post-process jtd-codegen TypeScript output.
fn post_process_typescript(code: &str, expected_class_name: &str) -> String {
    let rewritten = code.replace(ROOT_NAME_SENTINEL, expected_class_name);

    format!(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\
         //\n\
         // Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n{}",
        rewritten
    )
}

// =============================================================================
// Barrel file generation
// =============================================================================

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

fn schema_name_to_struct_name(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
    let last_segment = name.split('.').next_back().unwrap_or(name);

    if last_segment == "config" {
        let segments: Vec<&str> = name.split('.').collect();
        if segments.len() >= 2 {
            let processor_name = segments[segments.len() - 2];
            return format!("{}Config", to_pascal_case(processor_name));
        }
    }

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

fn schema_name_to_module_name(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
    name.replace('.', "_").to_lowercase()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_process_python_substitutes_root_sentinel() {
        let code = "@dataclass\nclass StreamlibCanonRoot:\n    @classmethod\n    def from_json_data(cls, data) -> 'StreamlibCanonRoot':\n        return cls()\n";
        let out = post_process_python(code, "H264DecoderConfig");
        assert!(out.contains("class H264DecoderConfig:"));
        assert!(out.contains("-> 'H264DecoderConfig':"));
        assert!(!out.contains("StreamlibCanonRoot"));
    }

    #[test]
    fn post_process_python_substitutes_sub_types_via_prefix() {
        let code = "class StreamlibCanonRootWhep:\n    pass\nclass StreamlibCanonRoot:\n    pass\n";
        let out = post_process_python(code, "WebrtcWhepConfig");
        assert!(out.contains("class WebrtcWhepConfigWhep:"));
        assert!(out.contains("class WebrtcWhepConfig:"));
        assert!(!out.contains("StreamlibCanonRoot"));
    }

    #[test]
    fn post_process_typescript_substitutes_root_sentinel() {
        let code = "export interface StreamlibCanonRoot {\n}\n";
        let out = post_process_typescript(code, "H264DecoderConfig");
        assert!(out.contains("export interface H264DecoderConfig {"));
        assert!(!out.contains("StreamlibCanonRoot"));
    }

    #[test]
    fn post_process_typescript_substitutes_discriminator_union() {
        let code = "export type StreamlibCanonRoot = StreamlibCanonRootBar | StreamlibCanonRootFoo;\nexport interface StreamlibCanonRootBar { op: \"bar\"; }\n";
        let out = post_process_typescript(code, "EscalateRequest");
        assert!(out.contains("export type EscalateRequest = EscalateRequestBar | EscalateRequestFoo;"));
        assert!(out.contains("export interface EscalateRequestBar"));
        assert!(!out.contains("StreamlibCanonRoot"));
    }

    #[test]
    fn post_process_rust_substitutes_root_sentinel() {
        let code = "// Code generated by jtd-codegen for Rust v0.4.1\n\nuse serde::{Deserialize, Serialize};\n\n#[derive(Serialize, Deserialize)]\npub struct StreamlibCanonRoot {}\n";
        let out = post_process_rust(code, "H264DecoderConfig").unwrap();
        assert!(out.contains("pub struct H264DecoderConfig {}"));
        assert!(!out.contains("StreamlibCanonRoot"));
    }

    #[test]
    fn schema_name_to_module_name_strips_version_suffix() {
        assert_eq!(
            schema_name_to_module_name("com.tatolab.videoframe@1.0.0"),
            "com_tatolab_videoframe"
        );
    }

    #[test]
    fn schema_name_to_module_name_handles_no_version() {
        assert_eq!(
            schema_name_to_module_name("com.tatolab.simple_passthrough.config"),
            "com_tatolab_simple_passthrough_config"
        );
    }

    #[test]
    fn schema_name_to_struct_name_handles_config_suffix() {
        assert_eq!(
            schema_name_to_struct_name("com.tatolab.camera.config@1.0.0"),
            "CameraConfig"
        );
    }

    #[test]
    fn schema_name_to_struct_name_handles_plain() {
        assert_eq!(
            schema_name_to_struct_name("com.tatolab.videoframe@1.0.0"),
            "Videoframe"
        );
    }

    #[test]
    fn to_pascal_case_basic() {
        assert_eq!(to_pascal_case("hello_world"), "HelloWorld");
        assert_eq!(to_pascal_case("hello-world"), "HelloWorld");
        assert_eq!(to_pascal_case("hello"), "Hello");
    }

    #[test]
    fn to_pascal_case_with_digits() {
        assert_eq!(to_pascal_case("h264_encoder"), "H264Encoder");
    }

    #[test]
    fn camel_to_snake_basic() {
        assert_eq!(camel_to_snake("camelCase"), "camel_case");
        assert_eq!(camel_to_snake("HTTPServer"), "h_t_t_p_server");
        assert_eq!(camel_to_snake("snake_case"), "snake_case");
    }

    #[test]
    fn strip_prefix_only_when_strictly_longer() {
        assert_eq!(strip_prefix("Foo", "Foo"), "Foo");
        assert_eq!(strip_prefix("FooBar", "Foo"), "Bar");
        assert_eq!(strip_prefix("Bar", "Foo"), "Bar");
    }

    #[test]
    fn extract_decl_name_handles_struct_and_enum() {
        assert_eq!(
            extract_decl_name("pub struct Foo {", "pub struct ").as_deref(),
            Some("Foo")
        );
        assert_eq!(
            extract_decl_name("pub enum Bar {", "pub enum ").as_deref(),
            Some("Bar")
        );
        assert_eq!(extract_decl_name("fn other()", "pub struct "), None);
    }

    #[test]
    fn extract_tag_attr_finds_serde_tag() {
        assert_eq!(
            extract_tag_attr("#[serde(tag = \"op\")]"),
            Some("op")
        );
        assert_eq!(extract_tag_attr("#[derive(Debug)]"), None);
    }

    #[test]
    fn generate_rust_mod_rs_emits_pub_mod_and_pub_use() {
        let modules = vec![
            ("foo".to_string(), "Foo".to_string()),
            ("bar_baz".to_string(), "BarBaz".to_string()),
        ];
        let out = generate_rust_mod_rs(&modules);
        assert!(out.contains("pub mod foo;"));
        assert!(out.contains("pub mod bar_baz;"));
        assert!(out.contains("pub use foo::Foo;"));
        assert!(out.contains("pub use bar_baz::BarBaz;"));
    }

    #[test]
    fn generate_python_init_py_emits_imports_and_all() {
        let modules = vec![
            ("foo".to_string(), "Foo".to_string()),
            ("bar".to_string(), "Bar".to_string()),
        ];
        let out = generate_python_init_py(&modules);
        assert!(out.contains("from .foo import Foo"));
        assert!(out.contains("from .bar import Bar"));
        assert!(out.contains("\"Foo\""));
        assert!(out.contains("\"Bar\""));
    }

    #[test]
    fn generate_typescript_index_ts_emits_re_exports() {
        let modules = vec![("foo".to_string(), "Foo".to_string())];
        let out = generate_typescript_index_ts(&modules);
        assert!(out.contains("export * from \"./foo.ts\";"));
    }
}
