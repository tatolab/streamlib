// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Package management commands.

use std::path::Path;

use anyhow::{Context, Result};
use streamlib::engine_internal::core::{InstalledPackageEntry, InstalledPackageManifest, ProjectConfig};
use streamlib::sdk::runtime::{
    extract_slpkg_to_cache, host_target_triple, BuildEvent, BuildEventSink, BuildOrchestrator,
    BuildPolicy, BuildRequest, BuildSource, BuildStream,
};
use streamlib::sdk::PolyglotBuildOrchestrator;
use streamlib_idents::{select_version, RegistryClient, RegistryConfig, SemVerRange};
use streamlib_pack::{assemble_artifact, AssembleOptions, AssembleTarget, CargoProfile, PathDepPolicy};

/// Routes the orchestrator's build diagnostics to the CLI's stdout/stderr
/// during `pkg install` (the engine default sink re-emits via `tracing`,
/// but the CLI prints progress directly for the interactive install flow).
struct CliBuildSink;

impl BuildEventSink for CliBuildSink {
    fn emit(&self, event: BuildEvent) {
        match event {
            BuildEvent::Started { language } => {
                println!("    [{language}] build started");
            }
            BuildEvent::Line { stream, line } => match stream {
                BuildStream::Stdout => println!("    {line}"),
                BuildStream::Stderr => eprintln!("    {line}"),
            },
            BuildEvent::Finished { language } => {
                println!("    [{language}] build finished");
            }
            // `BuildEvent` is `#[non_exhaustive]`; a future variant prints nothing.
            _ => {}
        }
    }
}

/// Install a package: a registry ref `@org/name[@version]`, a local `.slpkg`
/// path, or an HTTP URL. A registry ref resolves + downloads the source-only
/// `.slpkg` from the Gitea generic registry; the package is then built from
/// source by the orchestrator (`AlwaysBuild`), identical to runtime
/// `Strategy::Registry` resolution.
pub async fn install(source: &str) -> Result<()> {
    let slpkg_path = if source.starts_with('@') {
        resolve_registry_ref_to_temp_slpkg(source)?
    } else if source.starts_with("http://") || source.starts_with("https://") {
        // Download to temp file
        println!("Downloading {}...", source);
        let response = reqwest::get(source)
            .await
            .with_context(|| format!("Failed to download {}", source))?;

        if !response.status().is_success() {
            anyhow::bail!("Download failed: HTTP {} for {}", response.status(), source);
        }

        let bytes = response
            .bytes()
            .await
            .with_context(|| "Failed to read response body")?;

        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join("streamlib-pkg-download.slpkg");
        std::fs::write(&temp_path, &bytes)
            .with_context(|| format!("Failed to write temp file {}", temp_path.display()))?;
        temp_path
    } else {
        let path = std::path::PathBuf::from(source);
        if !path.exists() {
            anyhow::bail!("File not found: {}", source);
        }
        path
    };

    println!("Installing {}...", slpkg_path.display());

    // Extract to cache
    let cache_dir = extract_slpkg_to_cache(&slpkg_path)
        .map_err(|e| anyhow::anyhow!("Failed to extract package: {}", e))?;

    // Load project config from extracted cache to get metadata
    let config = ProjectConfig::load(&cache_dir)
        .map_err(|e| anyhow::anyhow!("Failed to load package config: {}", e))?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Package missing [package] section in streamlib.yaml"))?;

    // Provision the package's runtime via the build orchestrator — the same
    // `materialize` path module-load drives at runtime. For a Python package
    // this re-stages the extracted artifact and provisions its self-contained
    // venv at `{staged_package_dir}/.venv/bin/python` as the tail of the
    // materialize step. A package with no Python runtime is a no-op on the
    // venv tail; a Rust package re-builds its cdylib (cargo short-circuits
    // when clean). `AlwaysBuild` forces the venv provision regardless of any
    // prior stale staging.
    let has_python = config.processors.iter().any(|p| {
        matches!(
            p.runtime.language,
            streamlib_processor_schema::ProcessorLanguage::Python
        )
    });
    if has_python {
        println!("  Provisioning Python venv via the build orchestrator...");
    }
    let request = BuildRequest {
        package: streamlib_processor_schema::PackageRef::new(
            package.org.clone(),
            package.name.clone(),
        ),
        source: BuildSource::PackageDir(cache_dir.clone()),
        policy: BuildPolicy::AlwaysBuild,
        host_triple: host_target_triple().to_string(),
    };
    let orchestrator = PolyglotBuildOrchestrator::default();
    let sink = CliBuildSink;
    let staged = orchestrator
        .materialize(&request, &sink)
        .map_err(|e| anyhow::anyhow!("Failed to materialize package: {}", e))?;
    // The orchestrator stages into the package cache slot
    // (`cache/packages/<name>-<version>/`), the same slot
    // `extract_slpkg_to_cache` wrote to. This is order-safe: `materialize`
    // fully reads the source (assemble into a temp dir + provision the venv
    // there) before its closing `atomic_swap` wipes-and-replaces the slot,
    // so the extracted source is consumed before it's overwritten. Keep
    // using the returned staged path below.
    let cache_dir = staged.staged_dir;

    // Add to installed packages manifest. Identity is the canonical
    // `@org/name` PackageRef — the typed-key contract from #717 means
    // entries with the same short name from different orgs no longer
    // collide.
    let mut manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    let entry = InstalledPackageEntry {
        name: streamlib_processor_schema::PackageRef::new(
            package.org.clone(),
            package.name.clone(),
        ),
        version: package.version,
        description: package.description.clone(),
        installed_from: source.to_string(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        cache_dir: cache_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
    };

    manifest.add(entry);
    manifest
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save packages manifest: {}", e))?;

    println!();
    println!("Installed {} v{}", package.name, package.version);
    if let Some(desc) = &package.description {
        println!("  {}", desc);
    }
    println!("  Cache: {}", cache_dir.display());

    // Clean up the temp .slpkg we materialized (HTTP URL or registry ref).
    if source.starts_with('@')
        || source.starts_with("http://")
        || source.starts_with("https://")
    {
        let _ = std::fs::remove_file(&slpkg_path);
    }

    Ok(())
}

/// Build THIS package (the current working directory) into a source-only
/// `.slpkg`. Pure source bundling — no compilation, no prebuilt cdylib,
/// nothing path-related (the assembler refuses a path dep / path patch). The
/// artifact is a hand-off bundle; the consumer builds it from source.
pub fn build(output: Option<&Path>) -> Result<()> {
    let package_dir = std::env::current_dir().context("resolve current working directory")?;
    crate::commands::link::ensure_no_active_link_for_pack(&package_dir)?;
    let output_path = resolve_slpkg_output(&package_dir, output)?;
    let outcome = assemble_source_slpkg(&package_dir, &output_path)?;
    println!("Built source-only package: {}", output_path.display());
    println!("  {} v{}", outcome.package_name, outcome.package_version);
    if outcome.schemas > 0 {
        println!("  Schemas: {}", outcome.schemas);
    }
    if outcome.processors > 0 {
        println!("  Processors: {}", outcome.processors);
    }
    Ok(())
}

/// Publish THIS package (the current working directory) to the Gitea generic
/// registry. Always repacks a fresh source-only `.slpkg` to a temp file
/// (never trusts a pre-existing artifact), then uploads it by version.
/// Registry endpoint + token come from `STREAMLIB_REGISTRY_URL` /
/// `STREAMLIB_REGISTRY_TOKEN` (falling back to `GITEA_URL`).
pub fn publish() -> Result<()> {
    let package_dir = std::env::current_dir().context("resolve current working directory")?;
    crate::commands::link::ensure_no_active_link_for_pack(&package_dir)?;
    // Lightweight manifest read — package metadata only, NO dependency
    // resolution (which would require the registry just to read name/version).
    let config = streamlib_cargo_build::read_minimal_project_config(&package_dir)
        .context("Failed to read streamlib.yaml")?
        .ok_or_else(|| anyhow::anyhow!("no streamlib.yaml at {}", package_dir.display()))?;
    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;

    let registry = RegistryConfig::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "registry not configured: set STREAMLIB_REGISTRY_URL (e.g. http://localhost:3300) \
             and STREAMLIB_REGISTRY_TOKEN to publish"
        )
    })?;

    // Always repack fresh into a temp file — publish never trusts a
    // pre-existing artifact (pack runs independently, at any time).
    let tmp = tempfile::Builder::new()
        .prefix("streamlib-publish-")
        .suffix(".slpkg")
        .tempfile()
        .context("create temp .slpkg")?;
    let outcome = assemble_source_slpkg(&package_dir, tmp.path())?;
    let bytes = std::fs::read(tmp.path()).context("read packed .slpkg")?;

    let pkg_ref =
        streamlib_idents::PackageRef::new(package.org.clone(), package.name.clone());
    let client = RegistryClient::new(&registry);
    println!(
        "Publishing {} v{} ({} bytes) to {}…",
        outcome.package_name,
        outcome.package_version,
        bytes.len(),
        registry.base_url
    );
    let url = client
        .upload_slpkg(&pkg_ref, package.version, &bytes)
        .map_err(|e| anyhow::anyhow!("upload failed: {}", e))?;
    println!("Published → {url}");
    Ok(())
}

/// Remove THIS package's build/pack artifacts from the current working
/// directory: any `*.slpkg`, the prebuilt `lib/` dir, and the generated
/// `_generated_/` wire-vocabulary trees (root + `python/`). All are
/// regenerated on the next build/pack.
pub fn clean() -> Result<()> {
    let dir = std::env::current_dir().context("resolve current working directory")?;
    let mut removed: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("slpkg") {
                if std::fs::remove_file(&p).is_ok() {
                    removed.push(p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                }
            }
        }
    }
    let lib = dir.join("lib");
    if lib.is_dir() && std::fs::remove_dir_all(&lib).is_ok() {
        removed.push("lib/".to_string());
    }
    for cand in [dir.join("_generated_"), dir.join("python").join("_generated_")] {
        if cand.is_dir() && std::fs::remove_dir_all(&cand).is_ok() {
            let rel = cand.strip_prefix(&dir).unwrap_or(&cand);
            removed.push(format!("{}/", rel.display()));
        }
    }

    if removed.is_empty() {
        println!("Nothing to clean.");
    } else {
        println!("Removed: {}", removed.join(", "));
    }
    Ok(())
}

/// Resolve the default `.slpkg` output path (`{name}-{version}.slpkg` in the
/// package dir) when `--output` isn't given.
fn resolve_slpkg_output(package_dir: &Path, output: Option<&Path>) -> Result<std::path::PathBuf> {
    match output {
        Some(p) => Ok(p.to_path_buf()),
        None => {
            let config = streamlib_cargo_build::read_minimal_project_config(package_dir)
                .context("Failed to read streamlib.yaml")?
                .ok_or_else(|| anyhow::anyhow!("no streamlib.yaml at {}", package_dir.display()))?;
            let package = config
                .package
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;
            Ok(package_dir.join(format!("{}-{}.slpkg", package.name.as_str(), package.version)))
        }
    }
}

/// Assemble a source-only `.slpkg` at `output_path`. The `Slpkg` target makes
/// `assemble_artifact` ship source only (no cdylib build) and enforce the
/// no-path contract; `no_build` / `profile` are inert on this path.
fn assemble_source_slpkg(
    package_dir: &Path,
    output_path: &Path,
) -> Result<streamlib_pack::AssembleOutcome> {
    assemble_artifact(
        package_dir,
        &AssembleTarget::Slpkg(output_path.to_path_buf()),
        &AssembleOptions {
            no_build: false,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
        },
        &(),
    )
    .map_err(|e| anyhow::anyhow!("pack failed: {}", e))
}

/// Resolve a registry reference `@org/name[@version]` to a downloaded
/// `.slpkg` in a temp file. Lists the package's published versions, selects
/// the highest satisfying the (optional) version requirement, and downloads
/// that version's source-only `.slpkg`. The caller's extract + materialize
/// flow then builds it from source.
fn resolve_registry_ref_to_temp_slpkg(source: &str) -> Result<std::path::PathBuf> {
    let body = &source[1..]; // strip the leading '@'
    let (ref_str, version_req) = match body.split_once('@') {
        Some((r, v)) => (
            format!("@{r}"),
            SemVerRange::from_str(v)
                .map_err(|e| anyhow::anyhow!("invalid version '{v}' in '{source}': {e}"))?,
        ),
        None => (format!("@{body}"), SemVerRange::Any),
    };
    let pkg_ref = parse_canonical_package_ref(&ref_str)?;
    let registry = RegistryConfig::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "registry not configured: set STREAMLIB_REGISTRY_URL (e.g. http://localhost:3300) \
             to install '{source}' from the registry"
        )
    })?;
    let client = RegistryClient::new(&registry);
    let available = client
        .list_versions(&pkg_ref)
        .map_err(|e| anyhow::anyhow!("listing versions of {ref_str}: {e}"))?;
    let selected =
        select_version(&pkg_ref, &version_req, &available).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Resolving {ref_str} → {selected} from {}…", registry.base_url);
    let (bytes, url) = client
        .download_slpkg(&pkg_ref, selected)
        .map_err(|e| anyhow::anyhow!("downloading {ref_str}@{selected}: {e}"))?;
    println!("  fetched {url} ({} bytes)", bytes.len());
    let temp_path = std::env::temp_dir().join(format!(
        "streamlib-install-{}-{}.slpkg",
        pkg_ref.name.as_str(),
        selected
    ));
    std::fs::write(&temp_path, &bytes)
        .with_context(|| format!("writing downloaded slpkg to {}", temp_path.display()))?;
    Ok(temp_path)
}

/// Inspect a .slpkg package without installing it.
pub fn inspect(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("File not found: {}", path.display());
    }

    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Failed to read ZIP archive: {}", path.display()))?;

    // Find and read streamlib.yaml from the archive
    let yaml_content = {
        let mut entry = archive
            .by_name("streamlib.yaml")
            .with_context(|| "Package missing streamlib.yaml")?;
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut entry, &mut buf)?;
        buf
    };

    let config: ProjectConfig =
        serde_yaml::from_str(&yaml_content).with_context(|| "Failed to parse streamlib.yaml")?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Package missing [package] section"))?;

    println!("Package: {} v{}", package.name, package.version);
    if let Some(desc) = &package.description {
        println!("Description: {}", desc);
    }
    if let Some(sv) = &package.streamlib_version {
        println!("Requires: streamlib {}", sv);
    }

    if !config.processors.is_empty() {
        println!();
        println!("Processors ({}):", config.processors.len());
        for proc in &config.processors {
            println!("  {} v{}", proc.name, proc.version);
            if let Some(desc) = &proc.description {
                println!("    {}", desc);
            }
            println!("    Runtime:   {:?}", proc.runtime.language);
            println!("    Execution: {:?}", proc.execution);
            if !proc.inputs.is_empty() {
                println!("    Inputs:");
                for input in &proc.inputs {
                    println!("      - {} ({})", input.name, input.schema);
                }
            }
            if !proc.outputs.is_empty() {
                println!("    Outputs:");
                for output in &proc.outputs {
                    println!("      - {} ({})", output.name, output.schema);
                }
            }
            if let Some(config_ref) = &proc.config {
                println!("    Config:    {} ({})", config_ref.name, config_ref.schema);
            }
        }
    }

    // List files in archive
    println!();
    println!("Files ({}):", archive.len());
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            println!("  {}", entry.name());
        }
    }

    Ok(())
}

/// List installed packages.
pub fn list() -> Result<()> {
    let manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    if manifest.packages.is_empty() {
        println!("No packages installed.");
        println!();
        println!("Install a package with:");
        println!("  streamlib pkg install <path-to.slpkg>");
        return Ok(());
    }

    println!("Installed packages ({}):\n", manifest.packages.len());

    for pkg in &manifest.packages {
        println!("  {} v{}", pkg.name, pkg.version);
        if let Some(desc) = &pkg.description {
            println!("    {}", desc);
        }
        println!("    Installed: {}", pkg.installed_at);
        println!("    Source:    {}", pkg.installed_from);
        println!();
    }

    Ok(())
}

/// Remove an installed package. The argument must be the canonical
/// `@org/name` form — the typed-key contract introduced in #717 means
/// bare short names like `core` no longer disambiguate which package to
/// remove.
pub fn remove(name: &str) -> Result<()> {
    let package_ref = parse_canonical_package_ref(name)?;

    let mut manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    let entry = match manifest.remove_by_ref(&package_ref) {
        Some(e) => e,
        None => {
            anyhow::bail!("Package '{}' is not installed.", package_ref);
        }
    };

    // Delete cache directory
    let cache_dir = streamlib::engine_internal::core::get_cached_package_dir(&entry.cache_dir);
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)
            .with_context(|| format!("Failed to remove cache dir {}", cache_dir.display()))?;
        println!("Removed cache: {}", cache_dir.display());
    }

    // Save manifest
    manifest
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save packages manifest: {}", e))?;

    println!("Removed package '{}'.", package_ref);

    Ok(())
}

/// Convert a CLI-supplied canonical-form string (`@org/name`) into a
/// typed [`streamlib_processor_schema::PackageRef`] via the official
/// Deserialize path. Wraps the round-trip with a CLI-friendly error so
/// users see "expected `@org/name`" rather than a serde parse error.
fn parse_canonical_package_ref(arg: &str) -> Result<streamlib_processor_schema::PackageRef> {
    serde_yaml::from_value::<streamlib_processor_schema::PackageRef>(serde_yaml::Value::String(
        arg.to_string(),
    ))
    .with_context(|| {
        format!(
            "Invalid canonical package reference '{}'. Expected `@org/name` form (e.g. `@tatolab/core`).",
            arg
        )
    })
}
