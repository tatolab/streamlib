// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build-script helpers use println!/eprintln! for `cargo:` directives

//! Build-script helpers for crates that consume their own `_generated_/` tree.
//!
//! Crates carry a `streamlib.yaml` next to their `Cargo.toml`. At build time
//! each crate's `build.rs` calls [`run_for_rust_crate`], which:
//!
//! 1. Resolves the crate's `streamlib.yaml` dependency graph.
//! 2. Runs the JTD codegen pipeline into `$OUT_DIR/_generated_/`.
//! 3. Writes `$OUT_DIR/_generated_shim.rs` — a flat `include!`-able file
//!    whose `pub mod` declarations carry `#[path = ...]` attributes pointing
//!    at the OUT_DIR tree (so module resolution doesn't fall back to the
//!    crate's `src/` directory).
//! 4. Emits `cargo:rerun-if-changed=` directives for every schema YAML and
//!    every `streamlib.yaml` reachable through the dependency graph.
//!
//! Consumers `include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"))`
//! inside their `pub mod _generated_ { ... }` block in `lib.rs`.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use streamlib_idents::{ResolvedPackages, ResolverOptions};

use crate::{generate_from_resolved, RuntimeTarget};

/// Run codegen for the calling Rust crate.
///
/// Reads `CARGO_MANIFEST_DIR` and `OUT_DIR` from the build-script environment
/// and writes the tree + shim. Idempotent and incremental: Cargo skips the
/// re-run when none of the watched paths changed.
///
/// A crate whose `streamlib.yaml` declares no schemas still gets an empty
/// shim file so `include!` doesn't fail at compile time.
///
/// On error, prints a diagnostic to stderr and exits the build-script
/// process. Build-script failures are always fatal — propagating via
/// `Result` would force every consuming crate to declare `anyhow` as a
/// `[build-dependencies]` entry purely for the error type.
pub fn run_for_rust_crate() {
    if let Err(err) = run_for_rust_crate_inner() {
        eprintln!("error: streamlib_jtd_codegen build.rs helper failed: {:?}", err);
        std::process::exit(1);
    }
}

fn run_for_rust_crate_inner() -> Result<()> {
    let crate_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .context("CARGO_MANIFEST_DIR not set — run_for_rust_crate must be called from build.rs")?,
    );
    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR")
            .context("OUT_DIR not set — run_for_rust_crate must be called from build.rs")?,
    );

    let manifest_path = crate_dir.join("streamlib.yaml");
    if !manifest_path.exists() {
        anyhow::bail!(
            "Expected streamlib.yaml at {} — crates wiring streamlib_jtd_codegen::build_rs::run_for_rust_crate must carry their own manifest",
            manifest_path.display()
        );
    }

    let resolved = streamlib_idents::resolve_with(&crate_dir, &ResolverOptions::default())
        .context("Failed to resolve streamlib.yaml dependency graph")?;

    emit_rerun_directives(&crate_dir, &resolved);

    let gen_dir = out_dir.join("_generated_");
    let shim_path = out_dir.join("_generated_shim.rs");

    // Clean any prior run's output so renames / deletions land cleanly.
    if gen_dir.exists() {
        fs::remove_dir_all(&gen_dir)
            .with_context(|| format!("Failed to clean {}", gen_dir.display()))?;
    }
    fs::create_dir_all(&gen_dir)
        .with_context(|| format!("Failed to create {}", gen_dir.display()))?;

    generate_from_resolved(&resolved, RuntimeTarget::Rust, &gen_dir)
        .context("Codegen failed")?;

    write_rust_shim(&gen_dir, &shim_path)
        .context("Failed to write _generated_shim.rs")?;

    Ok(())
}

/// Emit `cargo:rerun-if-changed=` for every schema YAML and every manifest
/// reachable through the resolved dependency set.
fn emit_rerun_directives(crate_dir: &Path, resolved: &ResolvedPackages) {
    println!("cargo:rerun-if-changed={}", crate_dir.join("streamlib.yaml").display());

    let lock = crate_dir.join("streamlib.lock");
    if lock.exists() {
        println!("cargo:rerun-if-changed={}", lock.display());
    }

    for pkg in resolved.iter_all() {
        let pkg_manifest = pkg.root_dir.join("streamlib.yaml");
        println!("cargo:rerun-if-changed={}", pkg_manifest.display());
        for schema in &pkg.schema_files {
            println!("cargo:rerun-if-changed={}", schema.display());
        }
    }
}

/// Read the codegen-produced `<gen_dir>/mod.rs` and rewrite it as a shim
/// that `include!()` can load from anywhere.
///
/// `pub mod foo;` declarations in the original `mod.rs` are decorated with
/// `#[path = "<absolute path>"]` pointing at the OUT_DIR tree. Every other
/// line (header comments, `pub use ...` re-exports, blanks, existing
/// attributes like `#[allow(non_snake_case)]`) passes through verbatim.
fn write_rust_shim(gen_dir: &Path, shim_path: &Path) -> Result<()> {
    let top_mod = gen_dir.join("mod.rs");

    let abs_gen_dir = gen_dir
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", gen_dir.display()))?;

    let mut shim = String::from(
        "// Copyright (c) 2025 Jonathan Fontanez\n\
         // SPDX-License-Identifier: BUSL-1.1\n\n\
         // Generated by streamlib_jtd_codegen::build_rs. DO NOT EDIT.\n\n",
    );

    if !top_mod.exists() {
        // Crate's manifest declared no schemas — emit an empty shim so
        // `include!` still resolves.
        fs::write(shim_path, shim)?;
        return Ok(());
    }

    let mod_rs = fs::read_to_string(&top_mod)
        .with_context(|| format!("Failed to read {}", top_mod.display()))?;

    let mut pending_attrs: Vec<String> = Vec::new();

    for line in mod_rs.lines() {
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            // Flush any buffered attrs before the blank.
            for attr in pending_attrs.drain(..) {
                shim.push_str(&attr);
                shim.push('\n');
            }
            shim.push('\n');
            continue;
        }

        // Skip the codegen header (we write our own).
        if trimmed.starts_with("//") {
            continue;
        }

        // Buffer attributes so we can inject `#[path = ...]` before them on
        // the next `pub mod` declaration.
        if trimmed.starts_with("#[") {
            pending_attrs.push(line.to_string());
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("pub mod ") {
            let name = rest.trim_end_matches(';').trim();
            let target = locate_module_file(&abs_gen_dir, name).with_context(|| {
                format!(
                    "Codegen produced `pub mod {}` but neither {}/{}/mod.rs nor {}/{}.rs exists",
                    name,
                    abs_gen_dir.display(),
                    name,
                    abs_gen_dir.display(),
                    name
                )
            })?;
            // String-escape the absolute path as a Rust string literal.
            shim.push_str(&format!("#[path = {:?}]\n", target.to_string_lossy()));
            for attr in pending_attrs.drain(..) {
                shim.push_str(&attr);
                shim.push('\n');
            }
            shim.push_str(line);
            shim.push('\n');
            continue;
        }

        // `pub use ...`, any unrecognised line — pass through.
        for attr in pending_attrs.drain(..) {
            shim.push_str(&attr);
            shim.push('\n');
        }
        shim.push_str(line);
        shim.push('\n');
    }

    fs::write(shim_path, shim)
        .with_context(|| format!("Failed to write {}", shim_path.display()))?;
    Ok(())
}

fn locate_module_file(gen_dir: &Path, name: &str) -> Result<PathBuf> {
    let group = gen_dir.join(name).join("mod.rs");
    if group.exists() {
        return Ok(group);
    }
    let flat = gen_dir.join(format!("{}.rs", name));
    if flat.exists() {
        return Ok(flat);
    }
    anyhow::bail!("module `{}` not found under {}", name, gen_dir.display())
}
