// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Build tasks for StreamLib development.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

pub mod build_runtime;
pub mod check_boundaries;
pub mod check_control_contract_compatibility;
pub mod check_graph_document_node_types;
pub mod check_module_host_table_surface;
pub mod check_no_tatolab_namespace_package_init;
pub mod check_vendored_trees;
pub mod check_workspace_version_pins;
pub mod codec_proof_image_measurement;
pub mod contract_component_references;
pub(crate) mod contract_pin_types;
pub mod generate_built_in_node_classes;
pub mod generate_control_contract;
pub mod generate_module_host_table_c_header;
pub mod generate_python_pin_types;
pub mod generate_runtime_client;
pub mod normal_build_dep_graph;
pub mod psnr;
pub(crate) mod runtime_client_contract_schemas;
pub mod runtime_client_node_ui_manifest;
pub(crate) mod runtime_client_pin_types;
pub mod sign_the_carried_packages_with_a_developer_id;
pub mod stage_browser_host;
pub mod stage_core_projects;
pub mod stage_first_party_modules;
pub mod stage_runtime_client_package;
pub mod stage_runtime_unit_carried_packages;
pub mod stage_runtime_unit_code_checkers;
pub mod stage_runtime_unit_uv;
pub mod stage_sample_projects;

/// Tracked (and untracked-but-not-ignored) files under one repo-relative root.
///
/// `git ls-files` rather than a filesystem walk, for the reason every gate here
/// shares: CI walks a clean checkout, so "the files in the repo" is the
/// semantics meant, and the scan roots hold virtualenvs and build trees that
/// are not ours to gate. `-z` because a path containing a newline would
/// otherwise split into two entries and drop both from the scan.
pub fn list_repository_files_under(
    workspace_root: &Path,
    repo_relative_root: &str,
) -> Result<Vec<String>> {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .arg(repo_relative_root)
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("failed to run `git ls-files` for {repo_relative_root}"))?;

    anyhow::ensure!(
        output.status.success(),
        "`git ls-files {repo_relative_root}` failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    Ok(String::from_utf8(output.stdout)
        .with_context(|| format!("`git ls-files {repo_relative_root}` emitted non-UTF-8 paths"))?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Run `cargo metadata` for the workspace rooted at `manifest_dir` and return
/// the parsed resolve document.
///
/// `--locked` for the reason every other cargo invocation here carries it: a
/// gate that rewrites `Cargo.lock` as a side effect of reading the graph
/// reports on a graph the commit does not contain.
pub fn run_cargo_metadata_resolve_document(manifest_dir: &Path) -> Result<serde_json::Value> {
    let manifest_path = manifest_dir.join("Cargo.toml");
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(&manifest_path)
        .output()
        .with_context(|| format!("running cargo metadata at {}", manifest_path.display()))?;

    anyhow::ensure!(
        output.status.success(),
        "cargo metadata failed at {}: {}",
        manifest_path.display(),
        String::from_utf8_lossy(&output.stderr).trim(),
    );

    serde_json::from_slice(&output.stdout).context("parsing cargo metadata JSON")
}

/// Refuse a source-walking gate run that read no source at all.
///
/// A gate whose scan roots moved out from under it is indistinguishable from a
/// clean tree: both report zero violations. `unnoticed_consequence` names what
/// the gate would then let through, so the failure reads as the gate's own
/// contract rather than a generic count assertion. One sentence shape for every
/// gate, so a fifth one cannot invent a weaker phrasing.
pub fn ensure_source_walking_gate_read_source(
    gate_name: &str,
    scan_roots_description: &str,
    files_scanned: usize,
    unnoticed_consequence: &str,
) -> Result<()> {
    anyhow::ensure!(
        files_scanned > 0,
        "{gate_name} scanned 0 files under {scan_roots_description} — the scan roots \
         moved out from under the gate, which would let {unnoticed_consequence} unnoticed"
    );
    Ok(())
}

/// A gate that walks the source tree under the given root and reports violations.
type SourceWalkingGate = fn(&Path) -> Result<()>;

/// Every source-walking gate, paired with the subcommand name that runs it alone.
///
/// Each gate reads the tree and reports; none builds the workspace. That is what
/// lets one process run them all in well under a second, and why CI runs them as
/// a single job rather than one runner per gate.
const ALL_SOURCE_WALKING_GATES: &[(&str, SourceWalkingGate)] = &[
    ("check-boundaries", check_boundaries::run),
    ("check-vendored-trees", check_vendored_trees::run),
    (
        "check-module-host-table-surface",
        check_module_host_table_surface::run,
    ),
    (
        "generate-module-host-table-c-header --check",
        generate_module_host_table_c_header::check,
    ),
    (
        "check-workspace-version-pins",
        check_workspace_version_pins::run,
    ),
    (
        "check-no-tatolab-namespace-package-init",
        check_no_tatolab_namespace_package_init::run,
    ),
    (
        "check-graph-document-node-types",
        check_graph_document_node_types::run,
    ),
];

/// Run every source-walking gate, reporting all failures rather than the first.
///
/// A gate that bails on first failure hides the rest behind a re-run, which is the
/// one thing a consolidated job must not reintroduce: eight separate jobs at least
/// told you about eight separate breakages at once.
fn run_all_source_walking_gates(workspace_root: &Path) -> Result<()> {
    let mut failed_gate_names: Vec<&str> = Vec::new();

    for (gate_name, run_gate) in ALL_SOURCE_WALKING_GATES {
        match run_gate(workspace_root) {
            Ok(()) => tracing::info!("PASS  {gate_name}"),
            Err(gate_failure) => {
                tracing::error!("FAIL  {gate_name}: {gate_failure:#}");
                failed_gate_names.push(gate_name);
            }
        }
    }

    anyhow::ensure!(
        failed_gate_names.is_empty(),
        "{} of {} source-walking gates failed: {}",
        failed_gate_names.len(),
        ALL_SOURCE_WALKING_GATES.len(),
        failed_gate_names.join(", ")
    );

    tracing::info!(
        "all {} source-walking gates passed",
        ALL_SOURCE_WALKING_GATES.len()
    );
    Ok(())
}

/// Run one command from the workspace root, failing on a non-zero exit status.
fn run_local_ci_gate_command(
    workspace_root: &Path,
    gate_name: &str,
    program: &str,
    arguments: &[&str],
) -> Result<()> {
    let exit_status = std::process::Command::new(program)
        .args(arguments)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("failed to spawn `{program}` for {gate_name}"))?;

    anyhow::ensure!(exit_status.success(), "{gate_name} failed ({exit_status})");
    Ok(())
}

/// Run the gates CI runs, in the order CI runs them, reporting every failure.
///
/// The point is that a green run here means a green run on the PR. Any gate added
/// to CI without being added here breaks that promise, so the two lists are meant
/// to be read side by side against `.github/workflows/`.
fn run_local_ci_gates(workspace_root: &Path) -> Result<()> {
    let mut failed_gate_names: Vec<&str> = Vec::new();

    if let Err(gate_failure) = run_all_source_walking_gates(workspace_root) {
        tracing::error!("{gate_failure:#}");
        failed_gate_names.push("source-walking gates");
    }

    let shelled_out_gates: &[(&str, &str, &[&str])] = &[
        ("rustfmt", "cargo", &["fmt", "--all", "--check"]),
        // Default targets only. A test's `println!` is a test's business —
        // `no-print-macro` ignores `tests` directories, and `--all-targets` here
        // would deny what that rule deliberately allows.
        // Same exclusion as CI so this really does mirror it: `skia-bindings`
        // cannot build on a runner, and a local gate that lints more than CI
        // does is a gate whose result nobody can act on.
        (
            "clippy",
            "cargo",
            &[
                "clippy",
                "--locked",
                "--workspace",
                "--exclude",
                "streamlib-adapter-skia",
                "--no-deps",
            ],
        ),
        (
            "license headers",
            "bash",
            &["scripts/check-license-headers.sh"],
        ),
        (
            "structure lints",
            "bash",
            &["scripts/run-structure-lints.sh"],
        ),
        // Linux compiles in every built-in, so only Linux can render them all.
        #[cfg(target_os = "linux")]
        (
            "generated built-in node classes match the runtime's built-ins",
            "cargo",
            &[
                "run",
                "--locked",
                "-q",
                "-p",
                "xtask",
                "--",
                "generate-built-in-node-classes",
                "--check",
            ],
        ),
        // The selection is `.config/nextest.toml`'s `ci` profile, which `ci.yml`
        // runs over the same build: one list, read by both.
        (
            "deviceless unit and integration tests (nextest profile `ci`)",
            "cargo",
            &[
                "nextest",
                "run",
                "--locked",
                "--profile",
                "ci",
                "-p",
                "streamlib",
                "-p",
                "streamlib-macros",
                "-p",
                "streamlib-node-schema",
                "-p",
                "streamlib-ipc-types",
                "-p",
                "streamlib-surface-client",
                "-p",
                "streamlib-gpu-frame-import-client",
                "-p",
                "streamlib-error",
                "-p",
                "streamlib-runtime-client-contract",
                "-p",
                "streamlib-engine",
                "-p",
                "streamlib-media-builtins",
                "-p",
                "streamlib-consumer-rhi",
                "-p",
                "tatolab-cli",
                "-p",
                "streamlib-python-wheel",
                "-p",
                "tatolab-module-sdk",
                "-p",
                "tatolab-read-track",
                "-p",
                "streamlib-relay-track-reader",
                "-p",
                "streamlib-asset-media-facts",
                "--features",
                "streamlib-engine/hardware-tests,streamlib-media-builtins/hardware-tests,tatolab-module-sdk/pyo3,tatolab-module-sdk/function-node",
                "--lib",
                "--bins",
                "--tests",
            ],
        ),
        (
            "engine and media built-ins doctests",
            "cargo",
            &[
                "test",
                "--locked",
                "--doc",
                "-p",
                "streamlib-engine",
                "-p",
                "streamlib-media-builtins",
                "--features",
                "streamlib-engine/hardware-tests,streamlib-media-builtins/hardware-tests",
            ],
        ),
        // A shell, for the environment: the control plane's and `tatolabd`'s
        // tests run under one machine root, the contract's and the CLI's
        // without, and both runs share one build.
        (
            "tests under a test machine root (nextest profile `ci-machine-directories-under-a-test-root`)",
            "sh",
            &[
                "-c",
                "selection='-p streamlib-runtime-client-contract -p streamlib-api-server \
                 -p tatolabd -p tatolab-cli \
                 --features streamlib-runtime-client-contract/machine-directories-under-a-test-root \
                 --features streamlib-api-server/machine-directories-under-a-test-root \
                 --features tatolabd/machine-directories-under-a-test-root,tatolabd/hardware-tests \
                 --features tatolab-cli/machine-directories-under-a-test-root' \
                 && cargo nextest run --locked \
                 --profile ci-machine-directories-under-a-test-root $selection \
                 -E 'package(streamlib-runtime-client-contract) | package(tatolab-cli)' \
                 && test_machine_root=\"$(mktemp -d /tmp/tl-XXXXXX)\" \
                 && trap 'rm -rf \"$test_machine_root\"' EXIT \
                 && TATOLAB_TEST_MACHINE_ROOT=\"$test_machine_root\" cargo nextest run --locked \
                 --profile ci-machine-directories-under-a-test-root $selection \
                 -E 'package(streamlib-api-server) | package(tatolabd)'",
            ],
        ),
        // The dependency closure's licences, against `deny.toml`'s allowlist.
        // Not a source-walking gate: those are in-process tree walkers by
        // contract, and this shells out to a binary that is not part of the
        // toolchain — `cargo install cargo-deny@0.20.2 --locked` if the run
        // reports no such command, matching the version `ci.yml`
        // pins. `--locked` because cargo-deny will otherwise rewrite
        // `Cargo.lock` to resolve the graph and then report on the rewrite.
        // `--workspace` so a crate reached only from a workspace member nobody
        // builds locally is still in scope, and `-D license-not-encountered` so
        // an allowance whose last user left the graph fails rather than warns.
        //
        // Last, like CI runs it: it is the only entry here that resolves the
        // whole dependency graph, so a failure in it cannot cost the others
        // their report.
        (
            "cargo deny check licenses",
            "cargo",
            &[
                "deny",
                "--locked",
                "--workspace",
                "check",
                "licenses",
                "-D",
                "license-not-encountered",
            ],
        ),
    ];

    for (gate_name, program, arguments) in shelled_out_gates {
        tracing::info!("running {gate_name}");
        if let Err(gate_failure) =
            run_local_ci_gate_command(workspace_root, gate_name, program, arguments)
        {
            tracing::error!("{gate_failure:#}");
            failed_gate_names.push(gate_name);
        }
    }

    anyhow::ensure!(
        failed_gate_names.is_empty(),
        "{} local CI gate(s) failed: {}",
        failed_gate_names.len(),
        failed_gate_names.join(", ")
    );

    tracing::info!("all local CI gates passed");
    Ok(())
}

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "StreamLib development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Boundary-grep CI gate for the Vulkan RHI capability split. Fails on
    /// `ash`, raw `vulkanalia` outside RHI/adapter crates, cdylibs depending
    /// on the full `streamlib` crate, or privileged Vulkan calls outside
    /// the RHI.
    CheckBoundaries,

    /// CI gate keeping the module host table data-plane only: every field of
    /// `tatolab-module-sdk`'s `#[repr(C)]` table is plain data, a handle or an
    /// `extern "C"` verb over them — never an engine or RHI type — and the SDK
    /// depends on no engine crate. See [`check_module_host_table_surface`].
    CheckModuleHostTableSurface,

    /// Drift trip-wire for the vendored third-party trees — the vulkanalia
    /// fork (`vendor/tatolab-vulkanalia{,-sys,-vma}`): hashes each vendored
    /// crate dir and fails on any byte change vs. the recorded hash — the
    /// guard against accidental in-place edits (a workspace `cargo fmt --all`
    /// sweep is the classic cause). Deliberate re-vendors and recorded patches
    /// update the hashes in the same commit per
    /// `vendor/VULKANALIA-FORK.md`.
    CheckVendoredTrees,

    /// CI gate keeping every in-tree `{ path = "…", version = "…" }` requirement
    /// equal to `[workspace.package] version`. A release writes the workspace
    /// version and no dependency requirement, so the pins sit still while the
    /// crates move. Inside one minor line
    /// that is invisible (`^0.17.0` matches `0.17.1`); the next breaking bump makes
    /// the workspace unresolvable, because `^0.17.0` excludes `0.18.0`, and holds
    /// the release shut. `cargo metadata --no-deps` parses it clean, so only a
    /// real resolve catches it, and the first real resolve is on the release
    /// branch. `--fix` moves every drifted pin onto the workspace version; the
    /// release workflow calls it right after the bump.
    CheckWorkspaceVersionPins {
        /// Rewrite drifted pins instead of reporting them.
        #[arg(long)]
        fix: bool,
    },

    /// CI gate keeping `tatolab` a PEP 420 namespace package. Fails on any
    /// repository file ending in `tatolab/__init__.py`, and on any under the
    /// runtime unit's lend (`target/tatolab-runtime/lib/tatolab/lend`) when one
    /// is built. A node interpreter merges the lent `tatolab.runtime` with
    /// its venv's `tatolab.stream` only while no portion of the namespace is a
    /// regular package.
    CheckNoTatolabNamespacePackageInit,

    /// CI gate holding every graph document's package node types to the module
    /// that defines each class, as the packages' `tatolab.nodes` entry points
    /// state it. A type naming a node through its package's re-export
    /// (`tatolab.camera:CameraSource`) fails here; the runtime refuses it at load.
    CheckGraphDocumentNodeTypes,

    /// Run every source-walking gate in one process and report all failures.
    /// This is what CI's `source-gates` job runs; the per-gate subcommands stay
    /// for narrowing down a failure locally.
    CheckAllSourceGates,

    /// Run the gates CI runs, so a green run here predicts a green PR. Builds
    /// the workspace, so it is slower than `check-all-source-gates` alone.
    RunLocalCiGates,

    /// Build the runtime unit at `target/tatolab-runtime/`, the install
    /// prefix's own shape: `bin/tatolabd`, `bin/tatolab`, the lend at
    /// `lib/tatolab/lend/` — the directory holding `tatolab/runtime/`, which a
    /// node interpreter puts first on `PYTHONPATH` — and `wheel/`. Builds
    /// `sdk/streamlib-python-wheel` as a wheel with the pinned maturin into
    /// `wheel/`, replaces the lend with that wheel's contents, then builds
    /// `tatolabd` and `tatolab` and copies them into `bin/`. It first stages the
    /// platform's bundled Vulkan driver, so `_vulkan_driver/` lands beside
    /// `_engine`. The one build of the runtime unit for developers, CI and the
    /// installer; it installs and publishes nothing.
    ///
    /// Debug by default, the profile `maturin develop` builds; `--release`
    /// builds what a release ships. Each run mints the engine build nonce its
    /// two compiles share, so it recompiles the engine and everything above
    /// it, and the next plain `cargo build` recompiles them again.
    BuildRuntime {
        /// Build with optimizations, as a release is built.
        #[arg(long)]
        release: bool,
        /// Build both binaries with the `machine-directories-under-a-test-root`
        /// feature — the machine runtime lock, the state directory and the
        /// runtime directory under `TATOLAB_TEST_MACHINE_ROOT` — and mark the
        /// unit so, for the integration suite. Never for a release.
        #[arg(long)]
        machine_directories_under_a_test_root: bool,
        /// Sign every Mach-O inside the carried wheels and the carried Python with this
        /// `Developer ID Application: …` identity, which a keychain on the search list holds,
        /// so a bundle carrying them passes Apple's notary. macOS only.
        #[arg(long)]
        macos_developer_id_application_identity: Option<String>,
    },

    /// Download uv's release archive for this host at its pinned version,
    /// check its pinned sha256, and unpack `uv` into
    /// `target/runtime-unit-uv/`; `build-runtime` copies it into the unit's
    /// `bin/`. A second run whose archive still matches downloads nothing.
    /// See [`stage_runtime_unit_uv`].
    StageRuntimeUnitUv,

    /// Download the TypeScript compiler's and oxlint's archives for this host
    /// at their pinned versions, check their pinned sha256, and unpack both
    /// into `target/runtime-unit-code-checkers/`; `build-runtime` places them
    /// in the unit's `bin/`. See [`stage_runtime_unit_code_checkers`].
    StageRuntimeUnitCodeCheckers,

    /// Resolve the core modules' third-party requirements and the scaffold's Python for every
    /// platform the runtime unit ships, download each to learn its sha256, and rewrite
    /// `xtask/runtime_unit_carried_packages.toml`, which `build-runtime` places in the unit. See
    /// [`stage_runtime_unit_carried_packages`].
    PinRuntimeUnitCarriedPackages,

    /// Build the browser host and its CEF subprocess from
    /// `packages/tatolab-browser`'s own lockfile against the CEF distribution
    /// `CEF_PATH` names, and stage them with the CEF runtime into the runtime
    /// unit's `lib/tatolab/browser-host/`, which `build-runtime` also stages.
    /// Without `CEF_PATH` the unit carries no browser host. See
    /// [`stage_browser_host`].
    StageBrowserHost,

    /// The codec proof's scorer: PSNR of a decoded frame set against the
    /// references that produced it, and the vivid rig's channel-mean drift
    /// lock, with the three bug-injection modes that keep either gate
    /// non-vacuous. Pure image math over PNGs the rig's ffmpeg decoded from a
    /// track `tatolab-read-track` read — GPU-free, so it is tested in CI while
    /// the rig runs that feed it are not.
    #[command(subcommand)]
    Psnr(psnr::PsnrCommand),

    /// Regenerate `tatolab/stream/_built_in_nodes.py` — one Python class per
    /// built-in, with a `TypedDict` for its config — from the runtime's own
    /// descriptors. `--check` is the CI gate: it fails when the checked-in
    /// module differs from a regeneration. Linux only, the floor that compiles
    /// in every built-in. See [`generate_built_in_node_classes`].
    GenerateBuiltInNodeClasses {
        /// Fail when the checked-in module differs instead of rewriting it.
        #[arg(long)]
        check: bool,
    },

    /// Regenerate `runtime/streamlib-control-contract/contract.json`, the control contract
    /// every adapter, SDK and reference page is generated from, from the operation registry.
    /// `--check` is the CI gate: it fails when the checked-in file differs from a regeneration.
    /// See [`generate_control_contract`].
    GenerateControlContract {
        /// Fail when the checked-in file differs instead of rewriting it.
        #[arg(long)]
        check: bool,
    },

    /// CI gate on a change to the control contract file: fails when it breaks a client built
    /// against an earlier contract file — a removed operation or field, a newly required
    /// param, a narrowed type, a value added to a closed set in an answer — and still states
    /// that file's contract version, or when it changes and raises neither the version nor the
    /// revision. See [`check_control_contract_compatibility`].
    CheckControlContractCompatibility {
        /// The contract file the change is compared with: `main`'s.
        #[arg(long)]
        earlier_contract_file: std::path::PathBuf,
    },

    /// Regenerate the generated files of `sdk/tatolab-runtime-client`, the TypeScript client
    /// of the control API, from the committed contract file. `--check` is the CI gate: it
    /// fails when a checked-in generated file differs from a regeneration. See
    /// [`generate_runtime_client`].
    GenerateRuntimeClient {
        /// Fail when a checked-in file differs instead of rewriting it.
        #[arg(long)]
        check: bool,
    },

    /// Regenerate `tatolab/stream/_contract_pin_types.py`, the built-in pin types and a pin
    /// declaration's wire form in Python, from the committed contract file, on any OS.
    /// `--check` is the CI gate: it fails when the checked-in module differs from a
    /// regeneration. See [`generate_python_pin_types`].
    GeneratePythonPinTypes {
        /// Fail when the checked-in module differs instead of rewriting it.
        #[arg(long)]
        check: bool,
    },

    /// Regenerate `tatolab-module-sdk`'s `include/tatolab_module_host_table.h`
    /// from the table's Rust mirror. `--check` fails when the checked-in header
    /// differs from a regeneration; `check-all-source-gates` runs it. See
    /// [`generate_module_host_table_c_header`].
    GenerateModuleHostTableCHeader {
        /// Fail when the checked-in header differs instead of rewriting it.
        #[arg(long)]
        check: bool,
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
        Commands::CheckBoundaries => check_boundaries::run(&workspace_root()?)?,
        Commands::CheckModuleHostTableSurface => {
            check_module_host_table_surface::run(&workspace_root()?)?
        }
        Commands::CheckVendoredTrees => check_vendored_trees::run(&workspace_root()?)?,
        Commands::CheckWorkspaceVersionPins { fix } => {
            let workspace_root = workspace_root()?;
            if fix {
                check_workspace_version_pins::rewrite_version_pins_to_workspace_version(
                    &workspace_root,
                )?;
            } else {
                check_workspace_version_pins::run(&workspace_root)?;
            }
        }
        Commands::CheckNoTatolabNamespacePackageInit => {
            check_no_tatolab_namespace_package_init::run(&workspace_root()?)?
        }
        Commands::CheckGraphDocumentNodeTypes => {
            check_graph_document_node_types::run(&workspace_root()?)?
        }
        Commands::CheckAllSourceGates => run_all_source_walking_gates(&workspace_root()?)?,
        Commands::RunLocalCiGates => run_local_ci_gates(&workspace_root()?)?,
        Commands::BuildRuntime {
            release,
            machine_directories_under_a_test_root,
            macos_developer_id_application_identity,
        } => build_runtime::run(
            &workspace_root()?,
            build_runtime::RuntimeUnitBuildProfile::from_release_flag(release),
            build_runtime::RuntimeUnitMachineDirectories::from_machine_directories_under_a_test_root_flag(
                machine_directories_under_a_test_root,
            ),
            macos_developer_id_application_identity.as_deref(),
        )?,
        Commands::StageRuntimeUnitUv => {
            stage_runtime_unit_uv::stage_the_runtime_unit_uv(&workspace_root()?)?;
        }
        Commands::StageRuntimeUnitCodeCheckers => {
            stage_runtime_unit_code_checkers::stage_the_runtime_unit_code_checkers(
                &workspace_root()?,
            )?;
        }
        Commands::PinRuntimeUnitCarriedPackages => {
            let workspace_root = workspace_root()?;
            let uv = stage_runtime_unit_uv::stage_the_runtime_unit_uv(&workspace_root)?;
            stage_runtime_unit_carried_packages::pin_the_carried_packages(&workspace_root, &uv)?;
        }
        Commands::StageBrowserHost => {
            stage_browser_host::stage_the_browser_host(&workspace_root()?)?;
        }
        Commands::Psnr(psnr_command) => psnr::run(psnr_command)?,
        Commands::GenerateBuiltInNodeClasses { check } => {
            generate_built_in_node_classes::run(&workspace_root()?, check)?
        }
        Commands::GenerateControlContract { check } => {
            generate_control_contract::run(&workspace_root()?, check)?
        }
        Commands::CheckControlContractCompatibility {
            earlier_contract_file,
        } => check_control_contract_compatibility::run(&workspace_root()?, &earlier_contract_file)?,
        Commands::GenerateRuntimeClient { check } => {
            generate_runtime_client::run(&workspace_root()?, check)?
        }
        Commands::GeneratePythonPinTypes { check } => {
            generate_python_pin_types::run(&workspace_root()?, check)?
        }
        Commands::GenerateModuleHostTableCHeader { check } => {
            generate_module_host_table_c_header::run(&workspace_root()?, check)?
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
