// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The injected build seam.
//!
//! The engine is a pure substrate — it resolves where a package's
//! source lives and loads the staged result, but it NEVER shells out
//! to a toolchain (`cargo` / `uv` / `deno`). Building is an optional,
//! injected capability: a [`BuildOrchestrator`] the deployment wires in
//! (the default in-process [`PolyglotBuildOrchestrator`] from
//! `streamlib-build-orchestrator`, or a future IPC build-service impl).
//! A frozen `.slpkg`-only deployment injects none, and is therefore
//! compiler-free by construction.
//!
//! [`PolyglotBuildOrchestrator`]: https://docs.rs/streamlib-build-orchestrator

use std::path::PathBuf;

/// How a [`Strategy`]'s source should be (re)built before load.
///
/// Staleness for [`IfStale`] is decided by the build tool's OWN
/// incremental fingerprint (cargo / uv / deno), never by file mtime —
/// mtime misses transitive-dep, feature-flag, and toolchain changes,
/// which is the exact class of edit that this whole subsystem exists
/// to stop silently no-op'ing.
///
/// [`Strategy`]: super::Strategy
/// [`IfStale`]: BuildPolicy::IfStale
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPolicy {
    /// Load the staged artifact as-is; never invoke a build tool. The
    /// prod / `.slpkg` posture. A missing artifact is a hard error.
    NeverBuild,
    /// (Re)build iff the build tool's own fingerprint reports changed
    /// inputs. Near-instant when clean (the tool short-circuits). The
    /// dev / runtime-authoring default.
    IfStale,
    /// Invoke the build tool unconditionally (the tool may still
    /// short-circuit its actual compilation). For CI cold builds or
    /// callers that distrust the fingerprint.
    AlwaysBuild,
}

impl BuildPolicy {
    /// Whether this policy can ever require a [`BuildOrchestrator`].
    pub fn requires_orchestrator(self) -> bool {
        !matches!(self, BuildPolicy::NeverBuild)
    }
}

/// Where a [`BuildOrchestrator`] reads build inputs from. Constructed by
/// the engine's source resolver; orchestrators match it exhaustively, so
/// it is a closed set (a new source kind is a deliberate, breaking
/// addition that every orchestrator must then handle).
#[derive(Debug, Clone)]
pub enum BuildSource {
    /// A package directory holding `streamlib.yaml` plus the per-language
    /// implementation sources (`Cargo.toml`+`src/`, `python/`, `ts/`).
    PackageDir(PathBuf),
    /// A `.slpkg` archive. No compiler involved — extract only.
    SlpkgArchive(PathBuf),
    /// A remote URL a build-service orchestrator fetches **and builds**.
    /// Reserved seam for a future build daemon (`streamlibd`): the
    /// in-process default orchestrator rejects it. No in-tree [`Strategy`]
    /// emits this today — [`Strategy::Url`] fetches its `.slpkg` in the
    /// engine resolver (network-only) and routes the extracted directory
    /// through [`BuildSource::PackageDir`], so the engine never asks an
    /// orchestrator to fetch. This arm exists for the daemon case where
    /// fetching and building are one remote operation.
    ///
    /// [`Strategy`]: super::Strategy
    /// [`Strategy::Url`]: super::Strategy::Url
    Remote(String),
}

/// Whether a [`BuildSource::PackageDir`] is the user's own editable source
/// tree or an immutable orchestrator-managed extract. The resolver knows which
/// [`Strategy`] produced the dir; an orchestrator cannot infer it from the path
/// alone, so it is threaded here. It gates two orchestrator decisions that are
/// only sound for a self-contained dir: Rust build-once-reuse (a mutable
/// checkout may carry `path:` / workspace-inherited crate deps resolving
/// OUTSIDE the package dir, which a package-local content fingerprint cannot
/// see, so only cargo's own fingerprint is a complete staleness oracle there)
/// and build-scratch reclamation (a mutable checkout's `target/` is the user's
/// own cargo incremental state, never reclaimed).
///
/// [`Strategy`]: super::Strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageSourceProvenance {
    /// The package dir is the user's own editable checkout: a [`Strategy::Path`]
    /// dep, an active `streamlib link` checkout override, or an install-time
    /// `path:` / root source. Crate deps may resolve outside the dir, so the
    /// package-local fingerprint is not a complete oracle (cargo must always
    /// re-run) and `target/` belongs to the user.
    ///
    /// [`Strategy::Path`]: super::Strategy::Path
    MutableUserCheckout,
    /// The package dir is an orchestrator-managed extract: a `.slpkg`, a
    /// registry download, an installed-cache entry, or a git-rev-pinned
    /// checkout. Self-contained, so the package-local fingerprint fully gates
    /// reuse and `target/` is disposable build scratch.
    ImmutableManagedExtract,
}

/// Everything a [`BuildOrchestrator`] needs to materialize a loadable
/// staged package directory for one package. Constructed by the engine;
/// orchestrators (and their tests) read/build it, so it is a plain
/// struct.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    /// Canonical `@org/name` of the package being materialized.
    pub package: streamlib_idents::PackageRef,
    /// Where the build inputs live.
    pub source: BuildSource,
    /// Whether the source dir is the user's own editable tree or an immutable
    /// managed extract — gates Rust build-once-reuse and scratch reclamation.
    pub source_provenance: PackageSourceProvenance,
    /// Whether / when to (re)build.
    pub policy: BuildPolicy,
    /// Host target triple the staged cdylib must target
    /// (e.g. `x86_64-unknown-linux-gnu`).
    pub host_triple: String,
    /// The exact directory the orchestrator must stage this package into —
    /// computed by the engine via the installed-package slot seam
    /// ([`installed_package_slot_dir`]) at request-build time, so the
    /// orchestrator holds NO slot-path convention of its own. This is the
    /// write side of the write==read invariant: a locked run derives its
    /// read slot from the same seam, so the two agree byte-for-byte.
    ///
    /// [`installed_package_slot_dir`]: crate::core::installed_package_slot_dir
    pub staging_destination_slot_dir: PathBuf,
}

/// A successful [`BuildOrchestrator::materialize`] result. Constructed by
/// orchestrator impls, so not `#[non_exhaustive]`.
#[derive(Debug, Clone)]
pub struct StagedArtifact {
    /// Directory the engine loads from — holds `streamlib.yaml`,
    /// `schemas/`, and (for Rust-impl packages) `lib/<triple>/*.so`.
    pub staged_dir: PathBuf,
    /// Whether a (re)build actually ran. `false` means the build tool's
    /// fingerprint short-circuited or the artifact was already staged.
    pub rebuilt: bool,
}

/// Which child-process stream a [`BuildEvent::Line`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStream {
    Stdout,
    Stderr,
}

/// A structured progress / diagnostic event emitted during
/// materialization. The orchestrator pushes these through a
/// [`BuildEventSink`]; the engine re-emits them as
/// [`ModuleLoadEvent::BuildLog`] and via `tracing`.
///
/// [`ModuleLoadEvent::BuildLog`]: super::ModuleLoadEvent::BuildLog
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum BuildEvent {
    /// A per-language build step began (`"rust"` / `"python"` / `"deno"`).
    Started { language: &'static str },
    /// One line of build-tool output.
    Line { stream: BuildStream, line: String },
    /// A per-language build step finished.
    Finished { language: &'static str },
}

/// Sink the engine hands a [`BuildOrchestrator`] so build diagnostics
/// flow to `tracing` (the engine default) or to an IPC / event stream
/// (a daemon) — never directly to `stdout`/`stderr`.
pub trait BuildEventSink: Send + Sync {
    /// Record one build event.
    fn emit(&self, event: BuildEvent);
}

/// Per-failure-mode error a [`BuildOrchestrator`] can surface.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The orchestrator does not handle this [`BuildSource`] arm (e.g.
    /// an in-process builder handed a [`BuildSource::Remote`], or a
    /// no-compiler builder handed a [`BuildSource::PackageDir`]).
    #[error("build orchestrator does not support source: {0}")]
    UnsupportedSource(String),

    /// A required build tool isn't installed / on `PATH`. Surfaced by a
    /// preflight check before any build is attempted, so a missing
    /// toolchain fails fast with an actionable message rather than a raw
    /// spawn error mid-build.
    #[error("build tool '{tool}' (needed for {language}) not found: {hint}")]
    ToolNotAvailable {
        tool: String,
        language: String,
        hint: String,
    },

    /// A build tool exited non-zero. `detail` carries the actionable
    /// tail of the tool's output.
    #[error("{tool} build failed for '{package}': {detail}")]
    BuildFailed {
        tool: String,
        package: String,
        detail: String,
    },

    /// The registry holds an **incomplete / inconsistent release** of the
    /// pinned version: the package depends on crates that the release
    /// manifest for `release_version` does not list as published. Surfaced
    /// *before* cargo runs, so a half-published registry fails fast with the
    /// exact missing artifacts named — instead of a cryptic
    /// `failed to select a version for …` deep in cargo / `streamlib-macros`
    /// version unification.
    #[error(
        "incomplete release of {release_version}: the registry is missing \
         {missing} (required by '{package}'). {hint}"
    )]
    IncompleteRelease {
        package: String,
        release_version: String,
        /// Comma-joined `name@version` of the pins absent from the release.
        missing: String,
        hint: String,
    },

    /// I/O, staging, or any other materialization failure.
    #[error("materialize failed for '{package}': {detail}")]
    Other { package: String, detail: String },
}

/// The injected build seam: turn a [`BuildRequest`] into a staged,
/// loadable package directory.
///
/// The engine declares this trait and calls it via `spawn_blocking`;
/// concrete impls live OUTSIDE the engine and own all toolchain
/// invocation. `materialize` is **blocking and object-safe** by design —
/// a build is a coarse, one-shot operation, so threading a `Future`
/// through the seam buys nothing and would couple plugin loading to a
/// runtime handle. Callers that need it off-thread wrap it themselves
/// (the engine does, in [`Runner::add_module`]).
///
/// [`Runner::add_module`]: super::super::Runner::add_module
pub trait BuildOrchestrator: Send + Sync + 'static {
    /// Materialize (or confirm) a staged package directory for
    /// `request`. Build diagnostics go through `sink`, not `stdout`.
    fn materialize(
        &self,
        request: &BuildRequest,
        sink: &dyn BuildEventSink,
    ) -> std::result::Result<StagedArtifact, BuildError>;
}
