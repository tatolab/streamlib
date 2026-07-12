// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.
//!
//! For routine codegen, each Rust crate's `build.rs` invokes
//! `streamlib_jtd_codegen::build_rs::run_for_rust_crate` automatically.
//! This subcommand exists for ad-hoc generation and the Python / Deno
//! triggers (`setup.py` + `deno task setup`) that shell out to the CLI.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use streamlib_jtd_codegen::{generate, GenerateOptions, RuntimeTarget};

pub mod check_boundaries;
pub mod check_cdylib_reach;
pub mod check_consumer_rhi_repr;
pub mod check_device_wait_idle;
pub mod check_no_escalate_in_lifecycle;
pub mod check_no_inventory_submit;
pub mod check_no_reverse_dns;
pub mod check_no_streamlib_metadata;
pub mod check_package_version_drift;
pub mod check_processor_spec_new;
pub mod check_schema_versions;
pub mod lint_logging;
pub mod manifest_schema;

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "StreamLib development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate code from JTD schemas using jtd-codegen.
    ///
    /// Thin wrapper around `streamlib-jtd-codegen`. The same pipeline is also
    /// reachable as `streamlib generate` for non-Rust developers (no rustup
    /// required).
    GenerateSchemas {
        /// Target language (default: rust)
        #[arg(long, default_value = "rust")]
        runtime: RuntimeTarget,

        /// Output directory (required)
        #[arg(long)]
        output: PathBuf,

        /// `streamlib.yaml`-driven mode: directory containing the manifest.
        /// The resolver walks declared dependencies and codegen ingests the
        /// resulting set.
        #[arg(long, group = "input")]
        project_dir: Option<PathBuf>,

        /// Process a single schema file
        #[arg(long, group = "input")]
        schema_file: Option<PathBuf>,

        /// Process all .yaml files in a directory
        #[arg(long, group = "input")]
        schema_dir: Option<PathBuf>,
    },

    /// Ban ad-hoc logging in polyglot SDK library code (Python + TypeScript).
    /// Paired with the workspace clippy.toml `disallowed-macros` rule for Rust.
    LintLogging,

    /// Boundary-grep CI gate for the Vulkan RHI capability split. Fails on
    /// `ash`, raw `vulkanalia` outside RHI/adapter crates, cdylibs depending
    /// on the full `streamlib` crate, or privileged Vulkan calls outside
    /// the RHI. See `docs/architecture/subprocess-rhi-parity.md`.
    CheckBoundaries,

    /// CI gate for the package-as-publication-unit rule from milestone 10.
    /// Fails when any schema YAML declares a top-level `version` key
    /// (versioning lives in `streamlib.yaml`, not in individual schemas).
    /// See `docs/architecture/schema-identity-and-packaging.md`.
    CheckSchemaVersions,

    /// CI gate for #402's atomic cutover off language-native metadata.
    /// Fails on `[package.metadata.streamlib]`, `[tool.streamlib]`, or a
    /// top-level `streamlib` key in `deno.json` / `deno.jsonc`. The single
    /// source of truth is `streamlib.yaml`; see
    /// `docs/architecture/schema-identity-and-packaging.md` (anti-pattern 4).
    CheckNoStreamlibMetadata,

    /// CI gate for milestone-10's structured-identifier rule. Fails on
    /// legacy reverse-DNS schema literals (`com.tatolab.*`,
    /// `com.streamlib.*`) anywhere in live workspace code. Apple
    /// platform code (`*/apple/*`), test code (`#[cfg(test)]`,
    /// `tests/`, `*_test{s}.rs`), and Rust comments are allowed. See
    /// `docs/architecture/schema-identity-and-packaging.md`.
    CheckNoReverseDns,

    /// CI gate for #793's all-dynamic registration rule. Fails on any
    /// `inventory::submit!(FactoryRegistration { ... })` in live code —
    /// the `#[processor]` macro no longer emits one, and reintroducing
    /// the pattern would bypass the dynamic-load model from milestone
    /// `All-Dynamic Package Loading` (#20). `RuntimeInitHookRegistration`
    /// inventory submissions are unaffected — only `FactoryRegistration`
    /// is flagged.
    CheckNoInventorySubmit,

    /// CI gate for the structured-everywhere `ProcessorSpec` rule from
    /// #707. Fails on `ProcessorSpec::new("PascalCase", ...)` — every
    /// call site must take a structured `SchemaIdent` (built via
    /// `SchemaIdent::new(...)` or via the macro-emitted
    /// `<Module>::schema_ident()`).
    CheckProcessorSpecNew,

    /// CI gate for the cdylib-reachability invariant on engine `Host*`
    /// constructors. Fails when any constructor-class method
    /// (`new*` / `create*` / `from_*`) inside an `impl HostVulkan*`
    /// block in the engine RHI references `host_inner()` or
    /// `host_callbacks()` — those break the cdylib direct-call path
    /// documented at `docs/architecture/cdylib-reachability.md`.
    CheckCdylibReach,

    /// CI gate for the escalate-from-lifecycle ban (anti-pattern #1
    /// in `docs/architecture/cdylib-reachability.md`). Fails when
    /// any fn taking `&RuntimeContextFullAccess<'_>` (typically
    /// `setup` / `teardown` / `setup_inner` / `teardown_inner`) calls
    /// `.escalate(...)` in its body. The lifecycle dispatch already
    /// holds the escalate gate; re-entry panics at runtime via
    /// `EscalateGate::enter`. The xtask is defense-in-depth — catches
    /// the violation at PR review before the runtime panic fires.
    CheckNoEscalateInLifecycle,

    /// CI gate for issue #1039's consumer-rhi `#[repr(...)]` discipline.
    /// Fails when any `pub enum` in `libs/streamlib-consumer-rhi/src/`
    /// is missing an explicit `#[repr(...)]`, or when any
    /// `pub struct X(T)` scalar tuple newtype is missing
    /// `#[repr(transparent)]` / `#[repr(C)]`. Consumer-rhi POD types
    /// cross the plugin FFI boundary as bare scalars; their byte
    /// layout is part of the wire contract. See
    /// `docs/architecture/subprocess-rhi-parity.md`.
    CheckConsumerRhiRepr,

    /// CI gate for the `vkDeviceWaitIdle` threading discipline. Fails on any
    /// raw `device_wait_idle()` call in the engine outside the mutex-guarded
    /// `HostVulkanDevice::wait_idle` helper. `vkDeviceWaitIdle` is externally
    /// synchronized over the device + every queue it owns; a raw call that
    /// skips the per-queue mutexes races concurrent submits during
    /// multi-processor setup and crashes the driver (the validation layer
    /// reports `UNASSIGNED-Threading-Info`).
    CheckDeviceWaitIdle,

    /// Regenerate `schemas/streamlib.schema.json` from the Rust
    /// [`StreamlibYaml`](streamlib_processor_schema::StreamlibYaml) source of
    /// truth (#714). Editors with `yaml-language-server` consume this schema
    /// for autocomplete + lint on every `streamlib.yaml`.
    EmitManifestSchema,

    /// CI gate for the streamlib.yaml schema (#714). Three assertions:
    /// (1) committed schema matches what Rust currently emits, (2) every
    /// `streamlib.yaml` carries the `# yaml-language-server: $schema=...`
    /// header, (3) every `streamlib.yaml` validates against the schema.
    CheckManifestSchema,

    /// CI gate that every publishable package's `Cargo.toml`
    /// `[package].version` matches its `streamlib.yaml` `package.version`
    /// (the `.slpkg` semver — the single source of truth). Packages with no
    /// `Cargo.toml` (schema-only) and workspace-inherited versions are
    /// skipped. `--fix` rewrites each drifting `Cargo.toml` from its
    /// `streamlib.yaml`, so the bump workflow is "edit streamlib.yaml, run
    /// `--fix`" — never hand-edit `Cargo.toml`.
    CheckPackageVersionDrift {
        /// Rewrite each drifting `Cargo.toml` from its `streamlib.yaml`.
        #[arg(long)]
        fix: bool,
    },

    /// Strip dev-time path-flavor `patch:` entries from a crate's
    /// `streamlib.yaml` so the published manifest is path-free. Intended to
    /// run against a scratch copy of the crate before `cargo publish` (cargo
    /// bundles `streamlib.yaml` verbatim with no file-rewrite hook). The
    /// publish-side half of the static registry distribution; the resolver's
    /// `Registry` arm resolves the now-path-free dep from the registry. See
    /// `docs/architecture/static-registry.md`.
    StripPublishManifest {
        /// Directory containing the `streamlib.yaml` to strip in place.
        #[arg(long)]
        dir: PathBuf,
    },

    /// Emit a daemon-free STATIC registry tree (cargo sparse + pypi-simple +
    /// npm + `.slpkg` generic) for the current workspace release into a
    /// directory served identically over `file://` (slpkg, pypi) or a dumb
    /// static HTTP mount (cargo, npm). No registry daemon, no token. See
    /// `docs/architecture/static-registry.md`.
    #[command(subcommand)]
    StaticRegistry(StaticRegistryAction),
}

#[derive(Subcommand)]
enum StaticRegistryAction {
    /// Emit the full four-ecosystem tree into `--out`, flipped in atomically
    /// once the release manifest lands.
    Emit {
        /// Target directory for the served tree (built in a staging sibling
        /// and moved in atomically).
        #[arg(long)]
        out: PathBuf,
        /// `-dev.N` prerelease suffix for the release manifest + SDK
        /// artifacts (matches `--dev N` on the publish scripts). The cargo
        /// closure always emits at the crates' ACTUAL manifest versions —
        /// a dev closure emit expects the workspace manifests already
        /// bumped (the publish scripts' bump/restore convention).
        #[arg(long)]
        dev: Option<u32>,
        /// Absolute base URL the cargo + npm mounts are served at (sparse/npm
        /// are HTTP-only by spec; baked into config.json + packuments).
        #[arg(long, default_value = "http://127.0.0.1:8000")]
        base_url: String,
        /// Also package + emit the workspace release-closure crates into the
        /// cargo tree (heavy — off by default; the fork is always emitted).
        #[arg(long)]
        cargo_closure: bool,
        /// Skip the vulkanalia-fork cargo tree.
        #[arg(long)]
        no_cargo_fork: bool,
        /// Skip the pypi-simple tree.
        #[arg(long)]
        no_pypi: bool,
        /// Skip the npm tree.
        #[arg(long)]
        no_npm: bool,
        /// Skip the `.slpkg` store + release manifest.
        #[arg(long)]
        no_slpkg: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::GenerateSchemas {
            runtime,
            output,
            project_dir,
            schema_file,
            schema_dir,
        } => generate(GenerateOptions {
            runtime,
            output,
            project_dir,
            schema_file,
            schema_dir,
            workspace_root: workspace_root()?,
            write_lockfile: true,
        })?,
        Commands::LintLogging => lint_logging::run(&workspace_root()?)?,
        Commands::CheckBoundaries => check_boundaries::run(&workspace_root()?)?,
        Commands::CheckSchemaVersions => check_schema_versions::run(&workspace_root()?)?,
        Commands::CheckNoStreamlibMetadata => {
            check_no_streamlib_metadata::run(&workspace_root()?)?
        }
        Commands::CheckNoReverseDns => check_no_reverse_dns::run(&workspace_root()?)?,
        Commands::CheckNoInventorySubmit => check_no_inventory_submit::run(&workspace_root()?)?,
        Commands::CheckProcessorSpecNew => check_processor_spec_new::run(&workspace_root()?)?,
        Commands::CheckCdylibReach => check_cdylib_reach::run(&workspace_root()?)?,
        Commands::CheckNoEscalateInLifecycle => {
            check_no_escalate_in_lifecycle::run(&workspace_root()?)?
        }
        Commands::CheckConsumerRhiRepr => {
            check_consumer_rhi_repr::run(&workspace_root()?)?
        }
        Commands::CheckDeviceWaitIdle => check_device_wait_idle::run(&workspace_root()?)?,
        Commands::CheckPackageVersionDrift { fix } => {
            check_package_version_drift::run(&workspace_root()?, fix)?
        }
        Commands::EmitManifestSchema => manifest_schema::emit(&workspace_root()?)?,
        Commands::CheckManifestSchema => manifest_schema::check(&workspace_root()?)?,
        Commands::StripPublishManifest { dir } => {
            streamlib_pack::strip_path_patches_in_dir(&dir).with_context(|| {
                format!("stripping path patches from {}", dir.display())
            })?;
            tracing::info!(dir = %dir.display(), "stripped path-flavor patch entries from streamlib.yaml");
        }
        Commands::StaticRegistry(StaticRegistryAction::Emit {
            out,
            dev,
            base_url,
            cargo_closure,
            no_cargo_fork,
            no_pypi,
            no_npm,
            no_slpkg,
        }) => {
            use streamlib_pack::static_registry::{
                emit_static_registry, EmitEcosystems, EmitOptions,
            };
            emit_static_registry(&EmitOptions {
                workspace_root: workspace_root()?,
                out,
                base_url,
                dev,
                ecosystems: EmitEcosystems {
                    cargo_fork: !no_cargo_fork,
                    cargo_closure,
                    pypi: !no_pypi,
                    npm: !no_npm,
                    slpkg: !no_slpkg,
                },
            })?
        }
    }

    Ok(())
}

/// Get the workspace root directory.
pub fn workspace_root() -> Result<PathBuf> {
    let output = std::process::Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .context("Failed to run cargo locate-project")?;

    let path = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in cargo output")?
        .trim()
        .to_string();

    PathBuf::from(path)
        .parent()
        .map(|p| p.to_path_buf())
        .context("Failed to get workspace root")
}
