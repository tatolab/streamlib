// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

pub mod build_runtime;
pub mod check_boundaries;
pub mod check_bounded_apt_install;
pub mod check_clock_usage;
pub mod check_device_wait_idle;
pub mod check_iceoryx2_construction;
pub mod check_no_device_api_version_branch;
pub mod check_no_escalate_in_lifecycle;
pub mod check_no_in_process_placement;
pub mod check_no_inheritable_descriptor;
pub mod check_no_tatolab_namespace_package_init;
pub mod check_no_unbounded_cstr_from_ptr;
pub mod check_vendored_trees;
pub mod check_workspace_version_pins;
pub mod codec_proof_image_measurement;
pub mod generate_built_in_node_classes;
pub mod generate_third_party_notices;
pub mod lint_logging;
pub mod normal_build_dep_graph;
pub mod psnr;
pub mod source_call_site_scan;
pub mod stage_local_relay;
pub mod stage_runtime_unit_uv;
pub mod stage_watch_page;
pub mod watch_page_third_party_notices;

/// Rust source roots a workspace crate may hold: the classic `src/` and the
/// folder-backed `processors/`. `lint_logging` walks these by name rather
/// than descending from the crate root, so a source tree outside both is
/// invisible to it.
pub const RUST_CRATE_SOURCE_ROOT_DIR_NAMES: &[&str] = &["src", "processors"];

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

/// Refuse a gate run where one of its scan roots contributed no file.
///
/// A renamed or moved root leaves the others carrying the whole gate, which
/// reads identically to a clean tree.
pub fn ensure_every_source_walking_gate_scan_root_contributed(
    gate_name: &str,
    files_scanned_per_scan_root: &[(&str, usize)],
) -> Result<()> {
    for (scan_root, files_scanned) in files_scanned_per_scan_root {
        anyhow::ensure!(
            *files_scanned > 0,
            "{gate_name} scanned 0 files under {scan_root} — that scan root moved out from \
             under the gate"
        );
    }
    Ok(())
}

/// Workspace-relative paths git tracks under `scan_roots`.
///
/// A filesystem walk would descend `sdk/streamlib-python-wheel/.venv-pyright`
/// and every other build tree, gating third-party sources the project does not
/// own. `git ls-files` sees exactly what CI checks out.
pub fn tracked_files_under_scan_roots(
    workspace_root: &Path,
    scan_roots: &[&str],
    gate_name: &str,
) -> Result<Vec<PathBuf>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z", "--"])
        .args(scan_roots)
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("failed to run `git ls-files` for {gate_name}"))?;

    anyhow::ensure!(
        output.status.success(),
        "`git ls-files` failed ({}) — {gate_name} cannot enumerate its scan roots",
        output.status
    );

    let listing =
        String::from_utf8(output.stdout).context("`git ls-files` emitted a non-UTF-8 path")?;
    Ok(listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Every source-walking gate, paired with the subcommand name that runs it alone.
///
/// Each gate reads the tree and reports; none builds the workspace. That is what
/// lets one process run all fifteen in well under a second, and why CI runs them as
/// a single job rather than one runner per gate.
const ALL_SOURCE_WALKING_GATES: &[(&str, fn(&Path) -> Result<()>)] = &[
    ("lint-logging", lint_logging::run),
    ("check-boundaries", check_boundaries::run),
    ("check-vendored-trees", check_vendored_trees::run),
    (
        "check-no-in-process-placement",
        check_no_in_process_placement::run,
    ),
    (
        "check-no-escalate-in-lifecycle",
        check_no_escalate_in_lifecycle::run,
    ),
    ("check-device-wait-idle", check_device_wait_idle::run),
    (
        "check-no-device-api-version-branch",
        check_no_device_api_version_branch::run,
    ),
    (
        "check-iceoryx2-construction",
        check_iceoryx2_construction::run,
    ),
    (
        "check-no-unbounded-cstr-from-ptr",
        check_no_unbounded_cstr_from_ptr::run,
    ),
    ("check-clock-usage", check_clock_usage::run),
    (
        "check-no-inheritable-descriptor",
        check_no_inheritable_descriptor::run,
    ),
    ("check-bounded-apt-install", check_bounded_apt_install::run),
    (
        "check-workspace-version-pins",
        check_workspace_version_pins::run,
    ),
    (
        "check-no-tatolab-namespace-package-init",
        check_no_tatolab_namespace_package_init::run,
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
        // `lint-logging` exempts `tests` directories, and `--all-targets` here
        // would deny what that walk deliberately allows.
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
        // The relay the local adapter's and `tatolabd`'s tests start.
        (
            "the local relay staged",
            "cargo",
            &[
                "run",
                "--locked",
                "-q",
                "-p",
                "xtask",
                "--",
                "stage-local-relay",
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
                "streamlib-processor-schema",
                "-p",
                "streamlib-ipc-types",
                "-p",
                "streamlib-surface-client",
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
                "streamlib-relay-adapter-sdk",
                "-p",
                "streamlib-relay-adapter-local",
                "-p",
                "streamlib-relay-adapter-cloudflare",
                "-p",
                "streamlib-relay-adapter-moqpro",
                "-p",
                "tatolab-read-track",
                "-p",
                "streamlib-relay-track-reader",
                "--features",
                "streamlib-engine/hardware-tests,streamlib-media-builtins/hardware-tests",
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
    /// Ban ad-hoc logging in polyglot SDK library code (Python + TypeScript).
    /// Paired with the workspace clippy.toml `disallowed-macros` rule for Rust.
    LintLogging,

    /// Boundary-grep CI gate for the Vulkan RHI capability split. Fails on
    /// `ash`, raw `vulkanalia` outside RHI/adapter crates, cdylibs depending
    /// on the full `streamlib` crate, or privileged Vulkan calls outside
    /// the RHI. See `docs/architecture/subprocess-rhi-parity.md`.
    CheckBoundaries,

    /// CI gate for the helper-process-placement-only ruling. Fails on the
    /// vocabulary of the banned model anywhere in
    /// the engine tree or `docs/`; the banned patterns and the two escape
    /// hatches are enumerated in
    /// [`check_no_in_process_placement`]. Markdown and Rust doc comments are
    /// scanned on purpose: a violation can announce itself in a `//!` line.
    /// See `docs/decisions/helper-process-placement-only.md`.
    CheckNoInProcessPlacement,

    /// CI gate for the escalate-from-lifecycle ban. Fails when
    /// any fn taking `&RuntimeContextFullAccess<'_>` (typically
    /// `setup` / `teardown` / `setup_inner` / `teardown_inner`) calls
    /// `.escalate(...)` in its body. The lifecycle dispatch already
    /// holds the escalate gate; re-entry panics at runtime via
    /// `EscalateGate::enter`. The xtask is defense-in-depth — catches
    /// the violation at PR review before the runtime panic fires.
    CheckNoEscalateInLifecycle,

    /// CI gate for the `vkDeviceWaitIdle` threading discipline. Fails on any
    /// raw `device_wait_idle()` call in the engine outside the mutex-guarded
    /// `HostVulkanDevice::wait_idle` helper. `vkDeviceWaitIdle` is externally
    /// synchronized over the device + every queue it owns; a raw call that
    /// skips the per-queue mutexes races concurrent submits during
    /// multi-processor setup and crashes the driver (the validation layer
    /// reports `UNASSIGNED-Threading-Info`).
    CheckDeviceWaitIdle,

    /// CI gate for the Vulkan API-version contract. Fails on any read of a
    /// physical device's reported `apiVersion`. That value is not a capability
    /// report — MoltenVK clamps it to whatever the instance requested, so a
    /// probe reads the engine's own `vk::make_version(1, 4, 0)` back. The
    /// requested instance version is the floor that makes the promoted 1.3
    /// entry points resolve.
    CheckNoDeviceApiVersionBranch,

    /// CI gate for the engine-owned iceoryx2 domain. Fails on any
    /// `NodeBuilder::new()` or `publish_subscribe::<` under `runtime/`, `sdk/` or
    /// `adapters/` outside `iceoryx2/node.rs`, and on any `Config::global_config()`
    /// anywhere — tests and benches included, since a node in another domain hangs
    /// a test silently and a service without the user header counts no loss.
    CheckIceoryx2Construction,

    /// CI gate for the borrow-checked-C-string rule in the Vulkan RHI. Fails
    /// on any `CStr::from_ptr(<owner>.as_ptr())` under
    /// `runtime/streamlib-engine/src/vulkan/` or
    /// `runtime/streamlib-consumer-rhi/src/`. `CStr::from_ptr` returns an
    /// unbounded lifetime, so the borrow is never tied to the storage the
    /// pointer came from and survives it, a use-after-free.
    /// `vk::StringArray::as_cstr` borrows
    /// from `&self` and is the drop-in. A bare pointer argument owned by an
    /// external API is not flagged.
    CheckNoUnboundedCstrFromPtr,

    /// CI gate for close-on-exec at source. Fails on a raw `libc::dup` or
    /// `libc::pipe`, and on a `pipe2`, `epoll_create1`, `timerfd_create`,
    /// `eventfd` or `recvmsg` missing its close-on-exec flag, anywhere under
    /// `runtime/ sdk/ adapters/` — test code included. A descriptor created
    /// inheritable reaches every process the app spawns, and a grandchild
    /// holding a copy of the app's stdout keeps anything reading that output
    /// waiting after the app has died.
    CheckNoInheritableDescriptor,

    /// CI gate for the wall-clock allowlist. Fails on a wall-clock read
    /// (`SystemTime::now`, `Utc::now`, `time.time_ns`, `datetime.now`, …)
    /// anywhere under `runtime/ sdk/ adapters/ xtask/ packages/test-fixtures/`
    /// outside the surfaces the plan permits it on: log record `host_ts` and
    /// `source_ts`, log file naming, and the local relay's certificate
    /// validity. Monotonic is the
    /// only legal clock on the data plane — a wall-clock value and a media
    /// timestamp share a unit and are different quantities, so subtracting
    /// across them is always a bug. There is no per-line pragma: widening the
    /// list is a plan change. See
    /// `docs/decisions/one-monotonic-clock.md`.
    CheckClockUsage,

    /// CI gate keeping every apt install in CI behind
    /// `.github/actions/install-linux-engine-build-dependencies`. Fails on any
    /// `apt-get` under `.github/workflows/`, and on any step calling that
    /// action without `timeout-minutes`. An inline `apt-get update && apt-get
    /// install` has no wall-clock bound, and the mode that costs is a mirror
    /// that is slow rather than stalled — one measured run fetched 35.6 MB at
    /// 48 kB/s over 12m17s while every request made progress, so neither
    /// `Acquire::Retries` (nothing failed) nor `Acquire::http::Timeout`
    /// (nothing went idle) engaged. Composite-action steps cannot declare
    /// `timeout-minutes`, so the caller's step is the only place the native
    /// backstop can live.
    CheckBoundedAptInstall,

    /// Drift trip-wire for the vendored third-party trees — the vulkanalia
    /// fork (`vendor/tatolab-vulkanalia{,-sys,-vma}`): hashes each vendored
    /// crate dir and fails on any byte change vs. the recorded hash — the
    /// guard against accidental in-place edits (a workspace `cargo fmt --all`
    /// sweep is the classic cause). Deliberate re-vendors and recorded patches
    /// update the hashes in the same commit per
    /// `docs/architecture/vendored-vulkanalia.md`.
    CheckVendoredTrees,

    /// CI gate keeping every in-tree `{ path = "…", version = "…" }` requirement
    /// equal to `[workspace.package] version`. release-please's `simple` release
    /// type bumps the workspace version and ships no cargo dependency-requirement
    /// updater, so the pins sit still while the crates move. Inside one minor line
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
    /// is built. A processor interpreter merges the lent `tatolab.runtime` with
    /// its venv's `tatolab.stream` only while no portion of the namespace is a
    /// regular package.
    CheckNoTatolabNamespacePackageInit,

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
    /// processor interpreter puts first on `PYTHONPATH` — and `wheel/`. Builds
    /// `sdk/streamlib-python-wheel` as a wheel with the pinned maturin into
    /// `wheel/`, replaces the lend with that wheel's contents, then builds
    /// `tatolabd` and `tatolab` and copies them, and the local relay
    /// `stage-local-relay` builds, into `bin/`, and stages the preview page
    /// `stage-watch-page` builds into `lib/tatolab/watch/`. It first stages the
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
    },

    /// Build the machine's local relay, `moq-relay`, from its pinned crate
    /// with that crate's own lockfile into `target/local-relay/bin/`. It is no
    /// workspace member; `build-runtime` copies it into the unit's `bin/`, and
    /// the tests that start a local relay read it from there. A second run
    /// with the same pin builds nothing. See [`stage_local_relay`].
    StageLocalRelay,

    /// Download uv's release archive for this host at its pinned version,
    /// check its pinned sha256, and unpack `uv` into
    /// `target/runtime-unit-uv/`; `build-runtime` copies it into the unit's
    /// `bin/`. A second run whose archive still matches downloads nothing.
    /// See [`stage_runtime_unit_uv`].
    StageRuntimeUnitUv,

    /// Build the preview page from `runtime/tatolab-watch/` with the bun its
    /// `package.json` pins and the dependencies its `bun.lock` pins, into the
    /// runtime unit's `lib/tatolab/watch/`, which `build-runtime` also stages.
    /// See [`stage_watch_page`].
    StageWatchPage,

    /// The codec proof's scorer: PSNR of a decoded frame set against the
    /// references that produced it, and the vivid rig's channel-mean drift
    /// lock, with the three bug-injection modes that keep either gate
    /// non-vacuous. Pure image math over PNGs the rig's ffmpeg decoded from a
    /// track `tatolab-read-track` read — GPU-free, so it is tested in CI while
    /// the rig runs that feed it are not. See `docs/plan/ARCHITECTURE.md`
    /// §Media I/O.
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

    /// Regenerate `THIRD-PARTY-NOTICES.md` — the Rust closure's licence texts
    /// via `cargo about generate`, plus the vendored C++ projects that are not
    /// packages in the Cargo resolve graph and so reach the file only by being
    /// appended. Needs `cargo-about` installed and the network, which is why it
    /// is a command and not a gate; `cargo deny check licenses` is the half that
    /// runs on every PR. See [`generate_third_party_notices`] for the roster and
    /// why each project is on it.
    GenerateThirdPartyNotices {
        /// Generate for one standalone extension wheel under `packages/`
        /// instead of the engine workspace: its own closure, its own file, and
        /// no vendored C++ appendix.
        #[arg(long, value_name = "PACKAGE_DIRECTORY")]
        extension_package_directory: Option<PathBuf>,
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
        Commands::LintLogging => lint_logging::run(&workspace_root()?)?,
        Commands::CheckBoundaries => check_boundaries::run(&workspace_root()?)?,
        Commands::CheckNoInProcessPlacement => {
            check_no_in_process_placement::run(&workspace_root()?)?
        }
        Commands::CheckNoEscalateInLifecycle => {
            check_no_escalate_in_lifecycle::run(&workspace_root()?)?
        }
        Commands::CheckDeviceWaitIdle => check_device_wait_idle::run(&workspace_root()?)?,
        Commands::CheckNoDeviceApiVersionBranch => {
            check_no_device_api_version_branch::run(&workspace_root()?)?
        }
        Commands::CheckIceoryx2Construction => {
            check_iceoryx2_construction::run(&workspace_root()?)?
        }
        Commands::CheckNoUnboundedCstrFromPtr => {
            check_no_unbounded_cstr_from_ptr::run(&workspace_root()?)?
        }
        Commands::CheckClockUsage => check_clock_usage::run(&workspace_root()?)?,
        Commands::CheckNoInheritableDescriptor => {
            check_no_inheritable_descriptor::run(&workspace_root()?)?
        }
        Commands::CheckBoundedAptInstall => check_bounded_apt_install::run(&workspace_root()?)?,
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
        Commands::CheckAllSourceGates => run_all_source_walking_gates(&workspace_root()?)?,
        Commands::RunLocalCiGates => run_local_ci_gates(&workspace_root()?)?,
        Commands::BuildRuntime {
            release,
            machine_directories_under_a_test_root,
        } => build_runtime::run(
            &workspace_root()?,
            build_runtime::RuntimeUnitBuildProfile::from_release_flag(release),
            build_runtime::RuntimeUnitMachineDirectories::from_machine_directories_under_a_test_root_flag(
                machine_directories_under_a_test_root,
            ),
        )?,
        Commands::StageLocalRelay => {
            stage_local_relay::stage_the_local_relay(&workspace_root()?)?;
        }
        Commands::StageRuntimeUnitUv => {
            stage_runtime_unit_uv::stage_the_runtime_unit_uv(&workspace_root()?)?;
        }
        Commands::StageWatchPage => stage_watch_page::stage_the_watch_page(&workspace_root()?)?,
        Commands::Psnr(psnr_command) => psnr::run(psnr_command)?,
        Commands::GenerateBuiltInNodeClasses { check } => {
            generate_built_in_node_classes::run(&workspace_root()?, check)?
        }
        Commands::GenerateThirdPartyNotices {
            extension_package_directory,
        } => generate_third_party_notices::run(
            &workspace_root()?,
            &generate_third_party_notices::NoticesGenerationTarget::
                from_optional_extension_package_directory(extension_package_directory),
        )?,
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
