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

use streamlib_idents::{ResolvedPackage, ResolvedPackages, ResolverOptions, SemVer};

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
        let tasks = vec![SchemaTask {
            schema_path,
            package: None,
        }];
        return run_codegen_tasks(&tasks, runtime, &output);
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
        let tasks: Vec<SchemaTask> = files
            .into_iter()
            .map(|schema_path| SchemaTask {
                schema_path,
                package: None,
            })
            .collect();
        return run_codegen_tasks(&tasks, runtime, &output);
    }

    anyhow::bail!("No input specified. Use --project-dir, --schema-file, or --schema-dir");
}

/// Direct entry: run codegen against an already-resolved package set.
///
/// The output layout is mixed: schemas in package-flavor manifests with
/// new-shape `metadata.type` declarations land in
/// `output/<org>__<package>/<snake_type>.<ext>`; legacy schemas with
/// reverse-DNS `metadata.name` declarations stay flat at
/// `output/<reverse_dns>.<ext>`. The two co-exist during the staged
/// per-package carve-out migration (#401, then per-package issues).
pub fn generate_from_resolved(
    resolved: &ResolvedPackages,
    runtime: RuntimeTarget,
    output: &Path,
) -> Result<()> {
    let mut tasks: Vec<SchemaTask> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for pkg in resolved.iter_all() {
        let pkg_ctx = PackageContext::from_resolved(pkg);
        for schema_path in &pkg.schema_files {
            if !seen.insert(schema_path.clone()) {
                continue;
            }
            tasks.push(SchemaTask {
                schema_path: schema_path.clone(),
                package: pkg_ctx.clone(),
            });
        }
    }
    tasks.sort_by(|a, b| a.schema_path.cmp(&b.schema_path));

    if tasks.is_empty() {
        tracing::info!("No schemas to generate");
        return Ok(());
    }

    run_codegen_tasks(&tasks, runtime, output)
}

/// Per-schema codegen task: the schema's path + the package context the
/// resolver associated it with (None for orphan schemas in --schema-file or
/// --schema-dir mode).
#[derive(Debug, Clone)]
pub struct SchemaTask {
    pub schema_path: PathBuf,
    pub package: Option<PackageContext>,
}

/// Package context propagated from `streamlib.yaml`'s `package:` block to
/// codegen for a schema. The codegen needs `org`/`name`/`version` to derive
/// per-package output layout and the structured `SchemaIdent` const literal
/// it emits next to each generated type.
#[derive(Debug, Clone)]
pub struct PackageContext {
    pub org: String,
    pub name: String,
    pub version: SemVer,
}

impl PackageContext {
    fn from_resolved(pkg: &ResolvedPackage) -> Option<Self> {
        pkg.manifest.package.as_ref().map(|p| Self {
            org: p.org.as_str().to_string(),
            name: p.name.as_str().to_string(),
            version: p.version,
        })
    }
}

fn run_codegen_tasks(tasks: &[SchemaTask], runtime: RuntimeTarget, output: &Path) -> Result<()> {
    if tasks.is_empty() {
        tracing::info!("No schemas found");
        return Ok(());
    }
    tracing::info!("Found {} schemas", tasks.len());

    match runtime {
        RuntimeTarget::Rust => run_jtd_codegen_rust(tasks, output),
        RuntimeTarget::Python => run_jtd_codegen_python(tasks, output),
        RuntimeTarget::Typescript => run_jtd_codegen_typescript(tasks, output),
    }
}

/// Minimal JTD schema structure for extracting metadata.
#[derive(Debug, Deserialize)]
struct JtdSchema {
    metadata: JtdMetadata,
}

#[derive(Debug, Deserialize)]
struct JtdMetadata {
    /// Legacy joined-string identifier (`com.tatolab.videoframe`). Set on
    /// schemas that haven't migrated to per-package layout yet.
    #[serde(default)]
    name: Option<String>,
    /// New structured-identifier `type` segment (PascalCase, e.g. `VideoFrame`).
    /// Set on schemas living inside a package-flavor `streamlib.yaml`. The
    /// codegen derives the full `SchemaIdent { org, package, type, version }`
    /// from this plus the enclosing package context.
    #[serde(default, rename = "type")]
    type_name: Option<String>,
}

/// Module + struct names + optional per-package subdirectory derived from a
/// schema's metadata + the enclosing package context.
///
/// New-shape schemas (declaring `metadata.type`) live under
/// `output/<package_subdir>/<module_name>.<ext>`; legacy schemas (declaring
/// `metadata.name`) stay flat at `output/<module_name>.<ext>`.
#[derive(Debug, Clone)]
struct SchemaIdentity {
    /// Module name at its level (no path components). New: snake_case type
    /// name (`video_frame`); Old: full reverse-DNS module
    /// (`com_tatolab_videoframe`).
    module_name: String,
    /// Type/struct name. New: PascalCase (`VideoFrame`); Old: legacy rule
    /// (`Videoframe`).
    struct_name: String,
    /// `<org>__<package>` directory under `output/` for new-shape schemas;
    /// `None` for legacy flat schemas.
    package_subdir: Option<String>,
}

impl SchemaIdentity {
    fn output_path(&self, output_root: &Path, ext: &str) -> PathBuf {
        let mut p = output_root.to_path_buf();
        if let Some(subdir) = &self.package_subdir {
            p.push(subdir);
        }
        p.push(format!("{}.{}", self.module_name, ext));
        p
    }

    /// Unique tempdir key — the codegen runs in tempdirs named per task to
    /// avoid colliding when two schemas across packages happen to share a
    /// snake_case module name.
    fn temp_dir_key(&self) -> String {
        match &self.package_subdir {
            Some(subdir) => format!("{}__{}", subdir, self.module_name),
            None => self.module_name.clone(),
        }
    }
}

fn classify_schema(yaml_content: &str, package: Option<&PackageContext>) -> Result<SchemaIdentity> {
    let schema: JtdSchema = serde_yaml::from_str(yaml_content)
        .context("Failed to parse YAML metadata")?;

    if let Some(type_name) = &schema.metadata.type_name {
        let pkg = package.with_context(|| {
            format!(
                "Schema declares metadata.type = {} but is not part of a package-flavor streamlib.yaml; codegen needs the enclosing org/package/version to derive the structured SchemaIdent",
                type_name
            )
        })?;
        let snake_type = pascal_to_snake(type_name);
        let package_subdir = format!("{}__{}", pkg.org, pkg.name.replace('-', "_"));
        return Ok(SchemaIdentity {
            module_name: snake_type,
            struct_name: type_name.clone(),
            package_subdir: Some(package_subdir),
        });
    }

    if let Some(name) = &schema.metadata.name {
        let module_name = schema_name_to_module_name(name);
        let struct_name = schema_name_to_struct_name(name);
        return Ok(SchemaIdentity {
            module_name,
            struct_name,
            package_subdir: None,
        });
    }

    anyhow::bail!(
        "Schema metadata must declare either `type` (new shape, requires enclosing \
         package-flavor streamlib.yaml) or `name` (legacy reverse-DNS shape)"
    )
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
/// ordering, and write the result to `temp_dir`. Returns the (identity,
/// json_path, sentinel_table) tuple.
fn prepare_schema(
    task: &SchemaTask,
    temp_dir: &Path,
) -> Result<(SchemaIdentity, PathBuf, SentinelTable)> {
    let yaml_path = &task.schema_path;
    let yaml_content = fs::read_to_string(yaml_path)
        .with_context(|| format!("Failed to read {}", yaml_path.display()))?;

    let identity = classify_schema(&yaml_content, task.package.as_ref())
        .with_context(|| format!("Failed to classify {}", yaml_path.display()))?;

    // YAML → JSON value (mutable so we can run pre-passes on it).
    let mut json_value: serde_json::Value = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse YAML {}", yaml_path.display()))?;

    // For new-shape schemas, jtd-codegen requires `metadata.name` to be set —
    // strip our `metadata.type` field and synthesize `metadata.name` from the
    // type name so the codegen sees a coherent JTD schema. The structured
    // identifier is reconstructed at codegen-emit time from the package
    // context.
    if identity.package_subdir.is_some() {
        if let Some(metadata) = json_value.get_mut("metadata").and_then(|v| v.as_object_mut()) {
            metadata.remove("type");
            metadata.insert(
                "name".to_string(),
                serde_json::Value::String(identity.struct_name.clone()),
            );
        }
    }

    let mut sentinel_table = SentinelTable::default();
    sentinel::substitute(&mut json_value, &mut sentinel_table)
        .with_context(|| format!("Sentinel substitution failed for {}", yaml_path.display()))?;
    ordering::sort_object_keys_recursively(&mut json_value);

    let json_filename = format!("{}.json", identity.temp_dir_key());
    let json_path = temp_dir.join(&json_filename);
    let json_content =
        serde_json::to_string_pretty(&json_value).context("Failed to serialize to JSON")?;
    fs::write(&json_path, &json_content)
        .with_context(|| format!("Failed to write {}", json_path.display()))?;

    Ok((identity, json_path, sentinel_table))
}

// =============================================================================
// Rust codegen
// =============================================================================

fn run_jtd_codegen_rust(tasks: &[SchemaTask], output_dir: &Path) -> Result<()> {
    verify_jtd_codegen()?;

    fs::create_dir_all(output_dir).context("Failed to create output directory")?;
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    let mut entries: Vec<SchemaIdentity> = Vec::new();

    for task in tasks {
        tracing::info!("  Processing: {}", task.schema_path.display());

        let (identity, json_path, sentinel_table) = prepare_schema(task, temp_dir.path())?;

        let temp_rust_out = temp_dir.path().join(format!("rust_{}", identity.temp_dir_key()));
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
            anyhow::bail!("jtd-codegen failed for {}: {}", task.schema_path.display(), stderr);
        }

        let generated_mod = temp_rust_out.join("mod.rs");
        let generated_code = fs::read_to_string(&generated_mod).with_context(|| {
            format!("Failed to read generated code for {}", task.schema_path.display())
        })?;

        let processed_code = post_process_rust(&generated_code, &identity.struct_name)?;
        let restored_code = sentinel::restore_rust(&processed_code, &sentinel_table);

        let output_path = identity.output_path(output_dir, "rs");
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create directory {}", parent.display())
            })?;
        }
        fs::write(&output_path, restored_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        entries.push(identity);
    }

    write_rust_barrels(output_dir, &entries)?;

    tracing::info!(
        "Generated {} Rust modules in {}",
        entries.len(),
        output_dir.display()
    );

    Ok(())
}

// =============================================================================
// Python codegen
// =============================================================================

fn run_jtd_codegen_python(tasks: &[SchemaTask], output_dir: &Path) -> Result<()> {
    verify_jtd_codegen()?;

    if output_dir.exists() {
        fs::remove_dir_all(output_dir).context("Failed to clean output directory")?;
    }
    fs::create_dir_all(output_dir).context("Failed to create output directory")?;

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let mut entries: Vec<SchemaIdentity> = Vec::new();

    for task in tasks {
        tracing::info!("  Processing: {}", task.schema_path.display());

        let (identity, json_path, sentinel_table) = prepare_schema(task, temp_dir.path())?;

        let temp_python_out = temp_dir.path().join(format!("python_{}", identity.temp_dir_key()));
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
            anyhow::bail!("jtd-codegen failed for {}: {}", task.schema_path.display(), stderr);
        }

        let generated_init = temp_python_out.join("__init__.py");
        let python_code = fs::read_to_string(&generated_init).with_context(|| {
            format!(
                "Failed to read generated Python for {}",
                task.schema_path.display()
            )
        })?;

        // New-shape schemas (declaring `metadata.type` under a package-flavor
        // streamlib.yaml) carry a structured `SchemaIdent` on every emitted
        // class. Legacy `metadata.name`-shape schemas have no enclosing package
        // context to construct from and are emitted as plain dataclasses;
        // authors who reference them construct `SchemaIdent` directly until
        // #702 migrates them off reverse-DNS.
        let schema_ident_emit: Option<SchemaIdentEmit> = if identity.package_subdir.is_some() {
            task.package.as_ref().map(|p| SchemaIdentEmit {
                org: p.org.clone(),
                package: p.name.clone(),
                type_name: identity.struct_name.clone(),
                version: p.version.to_string(),
            })
        } else {
            None
        };
        let processed_code = post_process_python(
            &python_code,
            &identity.struct_name,
            schema_ident_emit.as_ref(),
        );
        let restored_code = sentinel::restore_python(&processed_code, &sentinel_table);

        let output_path = identity.output_path(output_dir, "py");
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create directory {}", parent.display())
            })?;
        }
        fs::write(&output_path, &restored_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        tracing::info!("    -> {} (class {})", output_path.display(), identity.struct_name);
        entries.push(identity);
    }

    write_python_barrels(output_dir, &entries)?;

    tracing::info!(
        "Generated {} Python modules in {}",
        entries.len(),
        output_dir.display()
    );

    Ok(())
}

// =============================================================================
// TypeScript codegen
// =============================================================================

fn run_jtd_codegen_typescript(tasks: &[SchemaTask], output_dir: &Path) -> Result<()> {
    verify_jtd_codegen()?;

    if output_dir.exists() {
        fs::remove_dir_all(output_dir).context("Failed to clean output directory")?;
    }
    fs::create_dir_all(output_dir).context("Failed to create output directory")?;

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let mut entries: Vec<SchemaIdentity> = Vec::new();

    for task in tasks {
        tracing::info!("  Processing: {}", task.schema_path.display());

        let (identity, json_path, sentinel_table) = prepare_schema(task, temp_dir.path())?;

        let temp_ts_out = temp_dir.path().join(format!("ts_{}", identity.temp_dir_key()));
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
            anyhow::bail!("jtd-codegen failed for {}: {}", task.schema_path.display(), stderr);
        }

        let generated_index = temp_ts_out.join("index.ts");
        let ts_code = fs::read_to_string(&generated_index).with_context(|| {
            format!(
                "Failed to read generated TypeScript for {}",
                task.schema_path.display()
            )
        })?;

        let processed_code = post_process_typescript(&ts_code, &identity.struct_name);
        let restored_code = sentinel::restore_typescript(&processed_code, &sentinel_table);

        let output_path = identity.output_path(output_dir, "ts");
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create directory {}", parent.display())
            })?;
        }
        fs::write(&output_path, &restored_code)
            .with_context(|| format!("Failed to write {}", output_path.display()))?;

        tracing::info!("    -> {} (interface {})", output_path.display(), identity.struct_name);
        entries.push(identity);
    }

    write_typescript_barrels(output_dir, &entries)?;

    tracing::info!(
        "Generated {} TypeScript modules in {}",
        entries.len(),
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

/// Codegen-emit shape for a structured schema identifier. Carries the four
/// segments the post-processors weave into generated language sources so a
/// generated class is self-identifying without an authoring decorator.
///
/// Constructed at the run-codegen call site from the schema's enclosing
/// `PackageContext` (org / package / version) and the schema's
/// `metadata.type` (PascalCase). Legacy `metadata.name`-shape schemas have
/// no enclosing package context and therefore no `SchemaIdentEmit`.
#[derive(Debug, Clone)]
struct SchemaIdentEmit {
    org: String,
    package: String,
    type_name: String,
    version: String,
}

/// Post-process jtd-codegen Python output.
///
/// Always strips the `ROOT_NAME_SENTINEL` placeholder and prepends the
/// project header. When `schema_ident` is `Some` (new-shape schemas with a
/// `metadata.type` and an enclosing package-flavor `streamlib.yaml`),
/// additionally injects a `__streamlib_schema_ident__: ClassVar[SchemaIdent]`
/// class attribute so `@input(schema=GeneratedClass)` resolves to a
/// structured `SchemaIdent` without an authoring `@schema` decorator (which
/// is intentionally not part of the SDK; see issue #704).
fn post_process_python(
    code: &str,
    expected_class_name: &str,
    schema_ident: Option<&SchemaIdentEmit>,
) -> String {
    let rewritten = code.replace(ROOT_NAME_SENTINEL, expected_class_name);

    let with_header = format!(
        "# Copyright (c) 2025 Jonathan Fontanez\n\
         # SPDX-License-Identifier: BUSL-1.1\n\
         #\n\
         # Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n\n{}",
        rewritten
    );

    match schema_ident {
        Some(ident) => inject_schema_ident_python(&with_header, expected_class_name, ident),
        None => with_header,
    }
}

/// Rewrite the generated Python code so the `from typing import …` line
/// imports `ClassVar` (adding it alphabetically when absent) and a
/// `from streamlib.schema_ident import SchemaIdent` line follows it.
///
/// jtd-codegen v0.4.1 emits a single `from typing import …` line whose
/// import set varies per schema (e.g. some include `List`, most don't).
/// Parsing the line preserves whatever set the codegen produced rather
/// than depending on a literal string match.
///
/// Falls back to prepending fresh imports below the project header when
/// no `from typing import` line is present (defensive — every jtd-codegen
/// v0.4.1 emit currently has one).
fn inject_typing_imports(code: &str) -> String {
    const SCHEMA_IDENT_IMPORT: &str = "from streamlib.schema_ident import SchemaIdent";
    let typing_prefix = "from typing import ";
    let mut output = String::with_capacity(code.len() + 64);
    let mut handled = false;

    for line in code.split_inclusive('\n') {
        if !handled && line.trim_start().starts_with(typing_prefix) {
            let trim_start = line.find(typing_prefix).unwrap();
            let prefix_end = trim_start + typing_prefix.len();
            // Keep any trailing `\n` (or trailing whitespace) on the original line.
            let (imports_str, line_tail) = match line[prefix_end..].find('\n') {
                Some(nl_idx) => (
                    &line[prefix_end..prefix_end + nl_idx],
                    &line[prefix_end + nl_idx..],
                ),
                None => (&line[prefix_end..], ""),
            };
            let mut names: Vec<String> = imports_str
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect();
            if !names.iter().any(|n| n == "ClassVar") {
                names.push("ClassVar".to_string());
            }
            names.sort();
            names.dedup();
            output.push_str(&line[..trim_start]);
            output.push_str(typing_prefix);
            output.push_str(&names.join(", "));
            output.push_str(line_tail);
            // Emit the SchemaIdent import on the next physical line.
            // `line_tail` already carries the original line's `\n`; if it
            // didn't, append one so the SchemaIdent import isn't glued to
            // the typing line.
            if !line_tail.contains('\n') {
                output.push('\n');
            }
            output.push_str(SCHEMA_IDENT_IMPORT);
            output.push('\n');
            handled = true;
            continue;
        }
        output.push_str(line);
    }

    if !handled {
        // Fallback: prepend imports immediately after the project header.
        let marker = "# Generated from JTD schema using jtd-codegen. DO NOT EDIT.\n";
        if let Some(idx) = output.find(marker) {
            let insert_at = idx + marker.len();
            let injection = format!("\nfrom typing import ClassVar\n{}\n", SCHEMA_IDENT_IMPORT);
            output.insert_str(insert_at, &injection);
        } else {
            // No project header either — prepend at the very top.
            output.insert_str(
                0,
                &format!("from typing import ClassVar\n{}\n", SCHEMA_IDENT_IMPORT),
            );
        }
    }

    output
}

/// Inject `__streamlib_schema_ident__` onto a generated Python dataclass.
///
/// Two surgeries on the post-`ROOT_NAME_SENTINEL`-substitution code:
///
/// 1. Extend the existing `from typing import …` line to include `ClassVar`
///    and add a `from streamlib.schema_ident import SchemaIdent` line right
///    after it. The typing import is always emitted by jtd-codegen v0.4.1
///    for new-shape schemas (they all have `Optional` fields).
/// 2. Find the first `class <expected_class_name>:` declaration and inject
///    the class attribute right after the optional class docstring (or
///    directly after the class line when there's no docstring), so the
///    attribute is the first non-docstring statement in the class body.
///    `ClassVar[SchemaIdent]` opts the attribute out of dataclass field
///    treatment.
fn inject_schema_ident_python(
    code: &str,
    class_name: &str,
    ident: &SchemaIdentEmit,
) -> String {
    // 1. Extend the existing `from typing import …` line to include `ClassVar`
    //    and inject `from streamlib.schema_ident import SchemaIdent` right
    //    after it. The exact set of imports varies per schema (some include
    //    `List`, some don't, etc.), so parse the line and rewrite it rather
    //    than literal-matching one specific shape.
    let with_imports = inject_typing_imports(code);

    // 2. Inject the class attribute after the class declaration + optional
    //    docstring. Locate the class declaration first.
    let class_marker = format!("class {}:", class_name);
    let class_idx = match with_imports.find(&class_marker) {
        Some(idx) => idx,
        None => return with_imports, // class line not found; emit unchanged
    };

    // Cursor sits at the newline that ends the `class X:` line.
    let mut cursor = class_idx + class_marker.len();
    let bytes = with_imports.as_bytes();

    // Advance past the trailing newline of the class declaration.
    if cursor < bytes.len() && bytes[cursor] == b'\n' {
        cursor += 1;
    }

    // Skip blank / whitespace-only lines between class line and any docstring.
    while cursor < bytes.len() {
        let line_end = with_imports[cursor..]
            .find('\n')
            .map(|n| cursor + n)
            .unwrap_or(bytes.len());
        let line = &with_imports[cursor..line_end];
        if line.trim().is_empty() {
            cursor = if line_end < bytes.len() { line_end + 1 } else { line_end };
        } else {
            break;
        }
    }

    // Detect and skip an optional triple-quoted docstring.
    if cursor + 3 <= bytes.len() {
        let leading = &with_imports[cursor..];
        let trimmed = leading.trim_start();
        let leading_ws_len = leading.len() - trimmed.len();
        let triple = if trimmed.starts_with("\"\"\"") {
            Some("\"\"\"")
        } else if trimmed.starts_with("'''") {
            Some("'''")
        } else {
            None
        };
        if let Some(triple) = triple {
            // Position of the docstring opener.
            let opener_start = cursor + leading_ws_len;
            let after_opener = opener_start + 3;
            // Find the matching closing triple-quote.
            if let Some(close_rel) = with_imports[after_opener..].find(triple) {
                let close_end = after_opener + close_rel + 3;
                // Advance cursor past the docstring's trailing newline.
                let line_end = with_imports[close_end..]
                    .find('\n')
                    .map(|n| close_end + n + 1)
                    .unwrap_or(bytes.len());
                cursor = line_end;
            }
        }
    }

    // Inject the class attribute at `cursor`. Indented to four spaces,
    // matching the dataclass body. Wrapped on multiple lines so the version
    // string and structural commas read cleanly even for the longest
    // identifiers. The injection ends with the closing-`)` line's newline
    // only — a separator blank line is emitted only when the source code
    // doesn't already have one at `cursor` (i.e., when there's no
    // class-docstring contributing the blank line). This keeps the
    // post-docstring case from accumulating two blank lines on every
    // regen.
    let injection_body = format!(
        "    __streamlib_schema_ident__: ClassVar[SchemaIdent] = SchemaIdent(\n        org=\"{}\",\n        package=\"{}\",\n        type_=\"{}\",\n        version=\"{}\",\n    )\n",
        ident.org, ident.package, ident.type_name, ident.version,
    );
    let needs_blank_separator = cursor < bytes.len() && bytes[cursor] != b'\n';
    let injection = if needs_blank_separator {
        format!("{}\n", injection_body)
    } else {
        injection_body
    };

    let mut result = String::with_capacity(with_imports.len() + injection.len());
    result.push_str(&with_imports[..cursor]);
    result.push_str(&injection);
    result.push_str(&with_imports[cursor..]);
    result
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

/// Group entries by their `package_subdir` (None = flat) into a deterministic
/// ordering: legacy-flat entries first (sorted), then each per-package group
/// (subdir-sorted, entries within each subdir-sorted).
fn group_entries(
    entries: &[SchemaIdentity],
) -> (Vec<SchemaIdentity>, std::collections::BTreeMap<String, Vec<SchemaIdentity>>) {
    let mut flat: Vec<SchemaIdentity> = Vec::new();
    let mut groups: std::collections::BTreeMap<String, Vec<SchemaIdentity>> =
        std::collections::BTreeMap::new();
    for e in entries {
        match &e.package_subdir {
            Some(subdir) => groups.entry(subdir.clone()).or_default().push(e.clone()),
            None => flat.push(e.clone()),
        }
    }
    flat.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    for v in groups.values_mut() {
        v.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    }
    (flat, groups)
}

fn write_rust_barrels(output_dir: &Path, entries: &[SchemaIdentity]) -> Result<()> {
    let (flat, groups) = group_entries(entries);

    for (subdir, group) in &groups {
        let mut content = String::from(
            "// Copyright (c) 2025 Jonathan Fontanez\n\
             // SPDX-License-Identifier: BUSL-1.1\n\n\
             //! Generated schema types. DO NOT EDIT.\n\n",
        );
        for e in group {
            content.push_str(&format!("pub mod {};\n", e.module_name));
        }
        content.push('\n');
        for e in group {
            content.push_str(&format!("pub use {}::{};\n", e.module_name, e.struct_name));
        }
        let path = output_dir.join(subdir).join("mod.rs");
        fs::write(&path, content)
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }

    let mut top = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\n\
         //! Generated schema types. DO NOT EDIT.\n\n",
    );
    for e in &flat {
        top.push_str(&format!("pub mod {};\n", e.module_name));
    }
    for subdir in groups.keys() {
        // The `<org>__<package>` separator uses double underscore so org and
        // package names containing single underscores remain unambiguous; the
        // resulting module name is non-snake_case by Rust convention. Allow
        // it on the generated declaration so consumers don't see the warning.
        top.push_str(&format!("#[allow(non_snake_case)]\npub mod {};\n", subdir));
    }
    top.push('\n');
    for e in &flat {
        top.push_str(&format!("pub use {}::{};\n", e.module_name, e.struct_name));
    }
    for (subdir, group) in &groups {
        for e in group {
            top.push_str(&format!("pub use {}::{};\n", subdir, e.struct_name));
        }
    }
    let mod_path = output_dir.join("mod.rs");
    fs::write(&mod_path, top).context("Failed to write mod.rs")?;
    Ok(())
}

fn write_python_barrels(output_dir: &Path, entries: &[SchemaIdentity]) -> Result<()> {
    let (flat, groups) = group_entries(entries);

    for (subdir, group) in &groups {
        let mut init_py = String::from(
            "# Copyright (c) 2025 Jonathan Fontanez\n\
             # SPDX-License-Identifier: BUSL-1.1\n\
             #\n\
             # Generated by jtd-codegen. DO NOT EDIT.\n\n",
        );
        for e in group {
            init_py.push_str(&format!("from .{} import {}\n", e.module_name, e.struct_name));
        }
        init_py.push_str("\n__all__ = [\n");
        for e in group {
            init_py.push_str(&format!("    \"{}\",\n", e.struct_name));
        }
        init_py.push_str("]\n");
        let path = output_dir.join(subdir).join("__init__.py");
        fs::write(&path, init_py)
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }

    let mut top = String::from(
        "# Copyright (c) 2025 Jonathan Fontanez\n\
         # SPDX-License-Identifier: BUSL-1.1\n\
         #\n\
         # Generated by jtd-codegen. DO NOT EDIT.\n\n",
    );
    for e in &flat {
        top.push_str(&format!("from .{} import {}\n", e.module_name, e.struct_name));
    }
    for (subdir, group) in &groups {
        for e in group {
            top.push_str(&format!("from .{} import {}\n", subdir, e.struct_name));
        }
    }
    top.push_str("\n__all__ = [\n");
    for e in &flat {
        top.push_str(&format!("    \"{}\",\n", e.struct_name));
    }
    for group in groups.values() {
        for e in group {
            top.push_str(&format!("    \"{}\",\n", e.struct_name));
        }
    }
    top.push_str("]\n");
    let init_path = output_dir.join("__init__.py");
    fs::write(&init_path, top).context("Failed to write __init__.py")?;
    Ok(())
}

fn write_typescript_barrels(output_dir: &Path, entries: &[SchemaIdentity]) -> Result<()> {
    let (flat, groups) = group_entries(entries);

    for (subdir, group) in &groups {
        let mut idx = String::from(
            "// Copyright (c) 2025 Jonathan Fontanez\n\
             // SPDX-License-Identifier: BUSL-1.1\n\
             //\n\
             // Generated by jtd-codegen. DO NOT EDIT.\n\n",
        );
        for e in group {
            idx.push_str(&format!("export * from \"./{}.ts\";\n", e.module_name));
        }
        let path = output_dir.join(subdir).join("index.ts");
        fs::write(&path, idx)
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }

    let mut top = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\
         //\n\
         // Generated by jtd-codegen. DO NOT EDIT.\n\n",
    );
    for e in &flat {
        top.push_str(&format!("export * from \"./{}.ts\";\n", e.module_name));
    }
    for subdir in groups.keys() {
        top.push_str(&format!("export * from \"./{}/index.ts\";\n", subdir));
    }
    let index_path = output_dir.join("index.ts");
    fs::write(&index_path, top).context("Failed to write index.ts")?;
    Ok(())
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

/// PascalCase or `H264Encoder`-style identifier → snake_case.
///
/// Rules:
/// - Insert `_` before each uppercase that follows a lowercase or after a
///   digit (so `VideoFrame` → `video_frame`, `H264Encoder` → `h264_encoder`).
/// - Lowercase every letter.
///
/// Acronym sequences (`HTTPServer`) are NOT specially handled — schemas in
/// this codebase don't use them, and the tests lock the simple rule.
fn pascal_to_snake(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = result.chars().last();
            let needs_underscore = prev
                .map(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                .unwrap_or(false);
            if needs_underscore {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c.to_ascii_lowercase());
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
        let out = post_process_python(code, "H264DecoderConfig", None);
        assert!(out.contains("class H264DecoderConfig:"));
        assert!(out.contains("-> 'H264DecoderConfig':"));
        assert!(!out.contains("StreamlibCanonRoot"));
    }

    #[test]
    fn post_process_python_substitutes_sub_types_via_prefix() {
        let code = "class StreamlibCanonRootWhep:\n    pass\nclass StreamlibCanonRoot:\n    pass\n";
        let out = post_process_python(code, "WebrtcWhepConfig", None);
        assert!(out.contains("class WebrtcWhepConfigWhep:"));
        assert!(out.contains("class WebrtcWhepConfig:"));
        assert!(!out.contains("StreamlibCanonRoot"));
    }

    #[test]
    fn post_process_python_legacy_schema_omits_schema_ident() {
        // Legacy `metadata.name`-shape schemas have no enclosing package
        // context. The Python emit stays a plain dataclass — no
        // `__streamlib_schema_ident__` injection, no `ClassVar`/`SchemaIdent`
        // imports added.
        let code = "from typing import Any, Dict, Optional, Union, get_args, get_origin\n\n@dataclass\nclass StreamlibCanonRoot:\n    \"\"\"Legacy schema\"\"\"\n\n    field_a: 'str'\n";
        let out = post_process_python(code, "LegacyConfig", None);
        assert!(!out.contains("__streamlib_schema_ident__"));
        assert!(!out.contains("ClassVar"));
        assert!(!out.contains("from streamlib.schema_ident"));
        assert!(out.contains("class LegacyConfig:"));
    }

    #[test]
    fn post_process_python_new_shape_emits_schema_ident_after_docstring() {
        // New-shape (`metadata.type` + package context) schemas grow a
        // ClassVar-typed `__streamlib_schema_ident__` attribute right after
        // the class docstring, plus the matching imports. Inserts BEFORE
        // the original blank line so the field annotations stay where they
        // were — preserving the visual separation.
        let code = "from typing import Any, Dict, Optional, Union, get_args, get_origin\n\n@dataclass\nclass StreamlibCanonRoot:\n    \"\"\"\n    Multi-line\n    description\n    \"\"\"\n\n    width: 'int'\n    \"\"\"width docstring\"\"\"\n";
        let ident = SchemaIdentEmit {
            org: "tatolab".to_string(),
            package: "core".to_string(),
            type_name: "VideoFrame".to_string(),
            version: "1.0.0".to_string(),
        };
        let out = post_process_python(code, "VideoFrame", Some(&ident));
        assert!(out.contains("from typing import Any, ClassVar, Dict, Optional, Union, get_args, get_origin"));
        assert!(out.contains("from streamlib.schema_ident import SchemaIdent"));
        assert!(out.contains("__streamlib_schema_ident__: ClassVar[SchemaIdent] = SchemaIdent("));
        assert!(out.contains("org=\"tatolab\""));
        assert!(out.contains("package=\"core\""));
        assert!(out.contains("type_=\"VideoFrame\""));
        assert!(out.contains("version=\"1.0.0\""));
        // The injection lands inside the class body — the SchemaIdent
        // construction sits between the docstring and the `width` field
        // (no `width:` line falls between the close-quote and the
        // injection start).
        let ident_idx = out.find("__streamlib_schema_ident__").unwrap();
        let docstring_close_idx = out
            .rfind("description\n    \"\"\"")
            .map(|idx| idx + "description\n    \"\"\"".len())
            .unwrap();
        let width_field_idx = out.find("width: 'int'").unwrap();
        assert!(docstring_close_idx < ident_idx);
        assert!(ident_idx < width_field_idx);
    }

    #[test]
    fn post_process_python_new_shape_handles_no_docstring() {
        // Defensive: if a future jtd-codegen emit drops the class docstring
        // for some reason, the schema_ident attribute lands directly after
        // the class line.
        let code = "from typing import Any, Dict, Optional, Union, get_args, get_origin\n\n@dataclass\nclass StreamlibCanonRoot:\n    width: 'int'\n";
        let ident = SchemaIdentEmit {
            org: "tatolab".to_string(),
            package: "core".to_string(),
            type_name: "VideoFrame".to_string(),
            version: "1.0.0".to_string(),
        };
        let out = post_process_python(code, "VideoFrame", Some(&ident));
        let class_line_idx = out.find("class VideoFrame:").unwrap();
        let ident_idx = out.find("__streamlib_schema_ident__").unwrap();
        let width_field_idx = out.find("width: 'int'").unwrap();
        assert!(class_line_idx < ident_idx);
        assert!(ident_idx < width_field_idx);
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
            schema_name_to_module_name("com.streamlib.h264_encoder.config@1.0.0"),
            "com_streamlib_h264_encoder_config"
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
            schema_name_to_struct_name("com.streamlib.escalate_request@1.0.0"),
            "EscalateRequest"
        );
    }

    #[test]
    fn pascal_to_snake_basic_types() {
        assert_eq!(pascal_to_snake("VideoFrame"), "video_frame");
        assert_eq!(pascal_to_snake("AudioFrame"), "audio_frame");
        assert_eq!(pascal_to_snake("EncodedVideoFrame"), "encoded_video_frame");
        assert_eq!(pascal_to_snake("EncodedAudioFrame"), "encoded_audio_frame");
    }

    #[test]
    fn pascal_to_snake_handles_digit_to_letter_boundary() {
        // H264Encoder: digit 4 then capital E → underscore inserted.
        assert_eq!(pascal_to_snake("H264Encoder"), "h264_encoder");
        assert_eq!(pascal_to_snake("H265DecoderConfig"), "h265_decoder_config");
    }

    #[test]
    fn pascal_to_snake_single_word() {
        assert_eq!(pascal_to_snake("Frame"), "frame");
    }

    #[test]
    fn classify_schema_new_shape_with_package_context() {
        let yaml = "metadata:\n  type: VideoFrame\nproperties: {}\n";
        let pkg = PackageContext {
            org: "tatolab".to_string(),
            name: "core".to_string(),
            version: SemVer::new(1, 0, 0),
        };
        let id = classify_schema(yaml, Some(&pkg)).unwrap();
        assert_eq!(id.module_name, "video_frame");
        assert_eq!(id.struct_name, "VideoFrame");
        assert_eq!(id.package_subdir.as_deref(), Some("tatolab__core"));
    }

    #[test]
    fn classify_schema_new_shape_dashes_in_package_become_underscores() {
        let yaml = "metadata:\n  type: ScreenCapture\nproperties: {}\n";
        let pkg = PackageContext {
            org: "tatolab".to_string(),
            name: "screen-capture".to_string(),
            version: SemVer::new(1, 0, 0),
        };
        let id = classify_schema(yaml, Some(&pkg)).unwrap();
        assert_eq!(id.package_subdir.as_deref(), Some("tatolab__screen_capture"));
    }

    #[test]
    fn classify_schema_legacy_shape_no_package_required() {
        let yaml = "metadata:\n  name: com.streamlib.h264_encoder.config\nproperties: {}\n";
        let id = classify_schema(yaml, None).unwrap();
        assert_eq!(id.module_name, "com_streamlib_h264_encoder_config");
        assert_eq!(id.struct_name, "H264EncoderConfig");
        assert!(id.package_subdir.is_none());
    }

    #[test]
    fn classify_schema_new_shape_without_package_errors() {
        let yaml = "metadata:\n  type: VideoFrame\nproperties: {}\n";
        let err = classify_schema(yaml, None).unwrap_err();
        assert!(format!("{}", err).contains("metadata.type"));
    }

    #[test]
    fn classify_schema_neither_name_nor_type_errors() {
        let yaml = "metadata:\n  description: nothing\nproperties: {}\n";
        let err = classify_schema(yaml, None).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("type") && msg.contains("name"));
    }

    #[test]
    fn schema_identity_output_path_per_package() {
        let id = SchemaIdentity {
            module_name: "video_frame".to_string(),
            struct_name: "VideoFrame".to_string(),
            package_subdir: Some("tatolab__core".to_string()),
        };
        let p = id.output_path(Path::new("/out"), "rs");
        assert_eq!(p, PathBuf::from("/out/tatolab__core/video_frame.rs"));
    }

    #[test]
    fn schema_identity_output_path_flat() {
        let id = SchemaIdentity {
            module_name: "com_streamlib_h264_encoder_config".to_string(),
            struct_name: "H264EncoderConfig".to_string(),
            package_subdir: None,
        };
        let p = id.output_path(Path::new("/out"), "rs");
        assert_eq!(p, PathBuf::from("/out/com_streamlib_h264_encoder_config.rs"));
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
    fn group_entries_separates_flat_and_per_package() {
        let entries = vec![
            SchemaIdentity {
                module_name: "video_frame".to_string(),
                struct_name: "VideoFrame".to_string(),
                package_subdir: Some("tatolab__core".to_string()),
            },
            SchemaIdentity {
                module_name: "com_streamlib_h264_encoder_config".to_string(),
                struct_name: "H264EncoderConfig".to_string(),
                package_subdir: None,
            },
            SchemaIdentity {
                module_name: "audio_frame".to_string(),
                struct_name: "AudioFrame".to_string(),
                package_subdir: Some("tatolab__core".to_string()),
            },
        ];
        let (flat, groups) = group_entries(&entries);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].struct_name, "H264EncoderConfig");
        assert_eq!(groups.len(), 1);
        let core = groups.get("tatolab__core").unwrap();
        assert_eq!(core.len(), 2);
        // Sorted within group by module_name
        assert_eq!(core[0].module_name, "audio_frame");
        assert_eq!(core[1].module_name, "video_frame");
    }

    #[test]
    fn write_rust_barrels_top_level_and_per_package() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("tatolab__core")).unwrap();

        let entries = vec![
            SchemaIdentity {
                module_name: "video_frame".to_string(),
                struct_name: "VideoFrame".to_string(),
                package_subdir: Some("tatolab__core".to_string()),
            },
            SchemaIdentity {
                module_name: "com_streamlib_h264_encoder_config".to_string(),
                struct_name: "H264EncoderConfig".to_string(),
                package_subdir: None,
            },
        ];
        write_rust_barrels(tmp.path(), &entries).unwrap();

        let top = std::fs::read_to_string(tmp.path().join("mod.rs")).unwrap();
        assert!(top.contains("pub mod com_streamlib_h264_encoder_config;"));
        assert!(top.contains("pub mod tatolab__core;"));
        assert!(top.contains("pub use com_streamlib_h264_encoder_config::H264EncoderConfig;"));
        assert!(top.contains("pub use tatolab__core::VideoFrame;"));

        let sub = std::fs::read_to_string(tmp.path().join("tatolab__core/mod.rs")).unwrap();
        assert!(sub.contains("pub mod video_frame;"));
        assert!(sub.contains("pub use video_frame::VideoFrame;"));
    }

    #[test]
    fn write_python_barrels_re_exports_from_subpackages() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("tatolab__core")).unwrap();

        let entries = vec![
            SchemaIdentity {
                module_name: "video_frame".to_string(),
                struct_name: "VideoFrame".to_string(),
                package_subdir: Some("tatolab__core".to_string()),
            },
            SchemaIdentity {
                module_name: "com_streamlib_h264_encoder_config".to_string(),
                struct_name: "H264EncoderConfig".to_string(),
                package_subdir: None,
            },
        ];
        write_python_barrels(tmp.path(), &entries).unwrap();

        let top = std::fs::read_to_string(tmp.path().join("__init__.py")).unwrap();
        assert!(top.contains("from .com_streamlib_h264_encoder_config import H264EncoderConfig"));
        assert!(top.contains("from .tatolab__core import VideoFrame"));

        let sub = std::fs::read_to_string(tmp.path().join("tatolab__core/__init__.py")).unwrap();
        assert!(sub.contains("from .video_frame import VideoFrame"));
        assert!(sub.contains("\"VideoFrame\""));
    }

    #[test]
    fn write_typescript_barrels_re_exports_subpackage_index() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("tatolab__core")).unwrap();

        let entries = vec![
            SchemaIdentity {
                module_name: "video_frame".to_string(),
                struct_name: "VideoFrame".to_string(),
                package_subdir: Some("tatolab__core".to_string()),
            },
            SchemaIdentity {
                module_name: "com_streamlib_h264_encoder_config".to_string(),
                struct_name: "H264EncoderConfig".to_string(),
                package_subdir: None,
            },
        ];
        write_typescript_barrels(tmp.path(), &entries).unwrap();

        let top = std::fs::read_to_string(tmp.path().join("index.ts")).unwrap();
        assert!(top.contains("export * from \"./com_streamlib_h264_encoder_config.ts\";"));
        assert!(top.contains("export * from \"./tatolab__core/index.ts\";"));

        let sub = std::fs::read_to_string(tmp.path().join("tatolab__core/index.ts")).unwrap();
        assert!(sub.contains("export * from \"./video_frame.ts\";"));
    }
}
