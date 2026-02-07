// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use zip::write::FileOptions;
use zip::ZipWriter;

use streamlib::core::config::ProjectConfig;

/// Pack a processor package into a .slpkg bundle.
pub fn pack(package_dir: &Path, output: Option<&Path>) -> Result<()> {
    // 1. Load and validate streamlib.yaml
    let config = ProjectConfig::load(package_dir).context("Failed to load streamlib.yaml")?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;

    if config.processors.is_empty() {
        anyhow::bail!("No processors defined in streamlib.yaml");
    }

    // 2. Determine output filename
    let output_filename = format!("{}-{}.slpkg", package.name, package.version);
    let output_path = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| package_dir.join(&output_filename));

    // 3. Collect files to bundle
    let mut files_to_bundle: Vec<(String, std::path::PathBuf)> = Vec::new();

    // Required: streamlib.yaml
    files_to_bundle.push((
        "streamlib.yaml".to_string(),
        package_dir.join("streamlib.yaml"),
    ));

    // Optional: pyproject.toml (Python)
    let pyproject = package_dir.join("pyproject.toml");
    if pyproject.exists() {
        files_to_bundle.push(("pyproject.toml".to_string(), pyproject));
    }

    // Optional: deno.json (TypeScript)
    let deno_json = package_dir.join("deno.json");
    if deno_json.exists() {
        files_to_bundle.push(("deno.json".to_string(), deno_json));
    }

    // Collect source files based on processor entrypoints
    // Python: "module:Class" → module.py
    // TypeScript: "file.ts:Class" → file.ts
    for proc in &config.processors {
        if let Some(entrypoint) = &proc.entrypoint {
            let source_file = match proc.runtime.language {
                streamlib_codegen_shared::ProcessorLanguage::Python => {
                    // "grayscale_processor:GrayscaleProcessor" → "grayscale_processor.py"
                    let module = entrypoint.split(':').next().unwrap_or(entrypoint);
                    format!("{}.py", module)
                }
                streamlib_codegen_shared::ProcessorLanguage::TypeScript => {
                    // "halftone_processor.ts:HalftoneProcessor" → "halftone_processor.ts"
                    entrypoint
                        .split(':')
                        .next()
                        .unwrap_or(entrypoint)
                        .to_string()
                }
                streamlib_codegen_shared::ProcessorLanguage::Rust => {
                    continue; // Rust processors don't have source files to bundle
                }
            };

            let source_path = package_dir.join(&source_file);
            if source_path.exists() {
                files_to_bundle.push((source_file, source_path));
            } else {
                anyhow::bail!(
                    "Processor '{}' entrypoint file not found: {}",
                    proc.name,
                    source_path.display()
                );
            }
        }
    }

    // 4. Create ZIP archive
    let file = File::create(&output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut zip = ZipWriter::new(file);
    let options = FileOptions::<()>::default().compression_method(zip::CompressionMethod::Deflated);

    let mut added_files = std::collections::HashSet::new();
    for (name, path) in files_to_bundle {
        if added_files.contains(&name) {
            continue; // Skip duplicates
        }
        added_files.insert(name.clone());

        let mut contents = Vec::new();
        File::open(&path)
            .with_context(|| format!("Failed to open {}", path.display()))?
            .read_to_end(&mut contents)?;

        zip.start_file(&name, options)?;
        zip.write_all(&contents)?;
    }

    zip.finish()?;

    println!("Created: {}", output_path.display());
    println!("  Package: {} v{}", package.name, package.version);
    println!("  Processors: {}", config.processors.len());
    for proc in &config.processors {
        println!("    - {}", proc.name);
    }

    Ok(())
}
