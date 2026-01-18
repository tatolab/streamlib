// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema management commands.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use streamlib_schema::{codegen, parser, SchemaDefinition, SchemaRegistry};

/// Configuration from streamlib.toml
#[derive(Debug, Deserialize, Default)]
struct StreamlibConfig {
    #[serde(default)]
    schemas: SchemasConfig,
}

#[derive(Debug, Deserialize, Default)]
struct SchemasConfig {
    #[serde(default)]
    remote: Vec<String>,
    #[serde(default)]
    local: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    registry: Option<String>,
    #[serde(default)]
    output: OutputConfig,
}

#[derive(Debug, Deserialize, Default)]
struct OutputConfig {
    rust: Option<String>,
    python: Option<String>,
    typescript: Option<String>,
}

/// Load streamlib.toml from current directory or parents.
fn load_config() -> Result<(PathBuf, StreamlibConfig)> {
    let mut current = std::env::current_dir()?;

    loop {
        let config_path = current.join("streamlib.toml");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: StreamlibConfig = toml::from_str(&content)
                .with_context(|| format!("Failed to parse {}", config_path.display()))?;
            return Ok((current, config));
        }

        if !current.pop() {
            // No streamlib.toml found, return defaults
            return Ok((std::env::current_dir()?, StreamlibConfig::default()));
        }
    }
}

/// Sync all schemas (fetch remote + codegen for all languages).
pub fn sync(lang: Option<&str>) -> Result<()> {
    let (project_root, config) = load_config()?;

    println!("Syncing schemas...");

    let mut schemas = Vec::new();

    // Load local schemas
    for local_path in &config.schemas.local {
        let path = project_root.join(local_path);
        if path.exists() {
            println!("  Loading local: {}", local_path);
            let schema = parser::parse_yaml_file(&path)
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            schemas.push(schema);
        } else {
            eprintln!("  Warning: Local schema not found: {}", local_path);
        }
    }

    // Handle remote schemas (for now, just check if they're cached)
    let mut registry = SchemaRegistry::new()?;
    for remote_name in &config.schemas.remote {
        match registry.resolve(remote_name) {
            Ok(schema) => {
                println!("  Found cached: {}", remote_name);
                schemas.push((*schema).clone());
            }
            Err(_) => {
                eprintln!(
                    "  Warning: Remote schema not cached: {} (run `streamlib schema add {}`)",
                    remote_name, remote_name
                );
            }
        }
    }

    if schemas.is_empty() {
        println!("No schemas to sync.");
        return Ok(());
    }

    // Generate code for each language
    let should_gen_rust = lang.is_none() || lang == Some("rust");
    let should_gen_python = lang.is_none() || lang == Some("python");
    let should_gen_typescript = lang.is_none() || lang == Some("typescript");

    if should_gen_rust {
        let output_dir = config
            .schemas
            .output
            .rust
            .as_deref()
            .unwrap_or("src/schemas");
        generate_rust_code(&project_root, output_dir, &schemas)?;
    }

    if should_gen_python {
        if let Some(output_dir) = &config.schemas.output.python {
            generate_python_code(&project_root, output_dir, &schemas)?;
        }
    }

    if should_gen_typescript {
        if let Some(output_dir) = &config.schemas.output.typescript {
            generate_typescript_code(&project_root, output_dir, &schemas)?;
        }
    }

    println!("Sync complete.");
    Ok(())
}

/// Generate Rust code for schemas.
fn generate_rust_code(
    project_root: &Path,
    output_dir: &str,
    schemas: &[SchemaDefinition],
) -> Result<()> {
    let output_path = project_root.join(output_dir);
    std::fs::create_dir_all(&output_path)?;

    println!("  Generating Rust code in {}/", output_dir);

    for schema in schemas {
        let code = codegen::generate_rust(schema)?;
        let filename = format!("{}.rs", schema.rust_module_name());
        let file_path = output_path.join(&filename);
        std::fs::write(&file_path, &code)?;
        println!("    {}", filename);
    }

    // Generate mod.rs
    let mod_rs = codegen::generate_mod_rs(schemas);
    std::fs::write(output_path.join("mod.rs"), &mod_rs)?;
    println!("    mod.rs");

    Ok(())
}

/// Generate Python code for schemas.
fn generate_python_code(
    project_root: &Path,
    output_dir: &str,
    schemas: &[SchemaDefinition],
) -> Result<()> {
    let output_path = project_root.join(output_dir);
    std::fs::create_dir_all(&output_path)?;

    println!("  Generating Python code in {}/", output_dir);

    for schema in schemas {
        let code = codegen::generate_python(schema)?;
        let filename = format!("{}.py", schema.rust_module_name());
        let file_path = output_path.join(&filename);
        std::fs::write(&file_path, &code)?;
        println!("    {}", filename);
    }

    // Generate __init__.py
    let init_py = codegen::generate_init_py(schemas);
    std::fs::write(output_path.join("__init__.py"), &init_py)?;
    println!("    __init__.py");

    Ok(())
}

/// Generate TypeScript code for schemas.
fn generate_typescript_code(
    project_root: &Path,
    output_dir: &str,
    schemas: &[SchemaDefinition],
) -> Result<()> {
    let output_path = project_root.join(output_dir);
    std::fs::create_dir_all(&output_path)?;

    println!("  Generating TypeScript code in {}/", output_dir);

    for schema in schemas {
        let code = codegen::generate_typescript(schema)?;
        let filename = format!("{}.ts", schema.rust_module_name());
        let file_path = output_path.join(&filename);
        std::fs::write(&file_path, &code)?;
        println!("    {}", filename);
    }

    // Generate index.ts
    let index_ts = codegen::generate_index_ts(schemas);
    std::fs::write(output_path.join("index.ts"), &index_ts)?;
    println!("    index.ts");

    Ok(())
}

/// Add a remote schema to streamlib.toml.
pub fn add(schema_name: &str) -> Result<()> {
    let (_project_root, config) = load_config()?;

    // Check if already added
    if config.schemas.remote.contains(&schema_name.to_string()) {
        println!("Schema '{}' is already in streamlib.toml", schema_name);
        return Ok(());
    }

    // Try to fetch from registry (for now, just validate the name format)
    if !schema_name.contains('@') {
        anyhow::bail!(
            "Invalid schema name '{}'. Expected format: org.domain.name@version",
            schema_name
        );
    }

    // Add to config (we'd need to modify the TOML file properly)
    println!("Adding schema '{}' to streamlib.toml...", schema_name);
    println!();
    println!("Please add the following to your streamlib.toml:");
    println!();
    println!("[schemas]");
    println!("remote = [");
    for existing in &config.schemas.remote {
        println!("    \"{}\",", existing);
    }
    println!("    \"{}\",", schema_name);
    println!("]");
    println!();
    println!("Then run: streamlib schema sync");

    Ok(())
}

/// Create a new local schema file.
pub fn new_schema(name: &str) -> Result<()> {
    let (project_root, _config) = load_config()?;

    // Ensure schemas directory exists
    let schemas_dir = project_root.join("schemas");
    std::fs::create_dir_all(&schemas_dir)?;

    // Create schema filename
    let filename = format!("{}.yaml", name.replace('.', "_"));
    let file_path = schemas_dir.join(&filename);

    if file_path.exists() {
        anyhow::bail!("Schema file already exists: {}", file_path.display());
    }

    // Generate template
    let template = format!(
        r#"# Schema: {}
# Edit this file to define your schema fields

name: com.example.{}
version: 1.0.0
description: "Description of your schema"

fields:
  - name: example_field
    type: string
    description: "An example string field"

  - name: count
    type: uint32
    description: "An example numeric field"

  # Nested object example:
  # - name: metadata
  #   type: object
  #   fields:
  #     - name: created_at
  #       type: int64
  #     - name: tags
  #       type: array<string>
"#,
        name, name
    );

    std::fs::write(&file_path, template)?;

    println!("Created schema template: {}", file_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit {} to define your fields", file_path.display());
    println!("  2. Add to streamlib.toml:");
    println!("     [schemas]");
    println!("     local = [\"schemas/{}\"]", filename);
    println!("  3. Run: streamlib schema sync");

    Ok(())
}

/// Validate local schema files.
pub fn validate() -> Result<()> {
    let (project_root, config) = load_config()?;

    println!("Validating schemas...");

    let mut errors = 0;

    for local_path in &config.schemas.local {
        let path = project_root.join(local_path);
        print!("  {} ... ", local_path);

        if !path.exists() {
            println!("NOT FOUND");
            errors += 1;
            continue;
        }

        match parser::parse_yaml_file(&path) {
            Ok(schema) => {
                println!("OK ({})", schema.full_name());
            }
            Err(e) => {
                println!("ERROR");
                eprintln!("    {}", e);
                errors += 1;
            }
        }
    }

    if errors > 0 {
        anyhow::bail!("{} schema(s) failed validation", errors);
    }

    println!("All schemas valid.");
    Ok(())
}

/// List all schemas (local and remote).
pub fn list() -> Result<()> {
    let (project_root, config) = load_config()?;

    println!("Schemas:");
    println!();

    if !config.schemas.local.is_empty() {
        println!("Local:");
        for local_path in &config.schemas.local {
            let path = project_root.join(local_path);
            if path.exists() {
                match parser::parse_yaml_file(&path) {
                    Ok(schema) => {
                        println!("  {} ({})", schema.full_name(), local_path);
                    }
                    Err(_) => {
                        println!("  {} (invalid)", local_path);
                    }
                }
            } else {
                println!("  {} (not found)", local_path);
            }
        }
        println!();
    }

    if !config.schemas.remote.is_empty() {
        println!("Remote:");
        for remote in &config.schemas.remote {
            println!("  {}", remote);
        }
        println!();
    }

    if config.schemas.local.is_empty() && config.schemas.remote.is_empty() {
        println!("  (none configured)");
        println!();
        println!("Add schemas to streamlib.toml:");
        println!("  [schemas]");
        println!("  local = [\"schemas/my-schema.yaml\"]");
        println!("  remote = [\"com.tatolab.videoframe@1.0.0\"]");
    }

    Ok(())
}
