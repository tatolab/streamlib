// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Regenerates a project's `THIRD-PARTY-NOTICES.md`.
//!
//! Two projects' shapes, one command. The engine workspace's file is the wheel's
//! and the SDK crate's; a standalone extension wheel under `packages/` is not a
//! workspace member, links no engine crate and vendors no C++, so it owes a file
//! of its own over its own closure. [`NoticesGenerationTarget`] is that choice,
//! and it decides the manifest, the destination and the appendix together.
//!
//! The preamble is written here rather than in `about.hbs`, because what a file
//! covers and how to regenerate it differ per project while the licence sections
//! below them do not — one template, one prose block per target.
//!
//! Two halves that no single tool covers. `cargo about generate` walks the
//! resolve graph and reproduces each crate's licence text; the vendored C++
//! projects appended here are invisible to it for one shared reason — none of
//! them is a package in that graph — even though every one ends up inside the
//! wheel. [`VENDORED_CPP_PROJECTS`] is the roster; this doc does not repeat it,
//! because a census in prose goes stale the moment the table grows.
//!
//! The notice source is an enum because the trees genuinely differ.
//! `shaderc-sys` extracts its C++ sources into its own build directory, each
//! with a licence file. The trees `vendor/tatolab-vulkanalia-vma/build.rs`
//! compiles `wrapper.cpp` against carry no licence file at all — their
//! copyright line exists only in the comment block heading a header. The
//! PipeWire/SPA headers are checked in here with their own `COPYING`.
//!
//! Some of the shaderc-side texts reach the generated half by accident:
//! `cargo about` scans a crate's own directory for licence files, and finds the
//! extracted sources under one. They are appended again regardless — there the
//! text is attributed to the crate `shaderc-sys`, and a reader needs to know
//! which upstream project each set of terms actually covers.
//!
//! Not a CI gate, and deliberately so. This needs `cargo-about` installed and
//! reaches the network for crates that ship no licence file of their own. The
//! check that runs on every pull request is `cargo deny check licenses` —
//! a generated file rots, and the gate is what stops a licence outside the
//! allowlist landing quietly in between regenerations.
//!
//! `cargo about`'s own `--fail` is not the backstop and is not passed: the
//! workspace's sixteen publishable crates carry `license-file` rather than an
//! SPDX `license`, so it would fail every run. A dependency whose expression
//! cannot be synthesised is warned about here and omitted from the output; what
//! refuses it is `cargo deny check licenses`, which reads the same crate as
//! `unlicensed`.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Named the same in every project that ships one. The engine's is generated at
/// the workspace root and reached from the wheel and the SDK crate by symlink so
/// that both ship this exact text rather than a copy that can drift from it; an
/// extension wheel's sits in its own package root, which is already its project
/// root.
const THIRD_PARTY_NOTICES_FILE_NAME: &str = "THIRD-PARTY-NOTICES.md";

/// The opening paragraph, true of every project that ships this file.
const NOTICES_PREAMBLE_OPENING: &str = r"# Third-party notices

StreamLib distributes third-party code. This file reproduces each dependency's
copyright notice and licence text as of its last regeneration, as BSD-3 and MIT
require of a binary distribution and as Apache-2.0 §4(d) requires of upstream
`NOTICE` contents. Regeneration is a manual step, so a dependency added since
then may not be listed — please report one rather than assume it was excluded
deliberately.
";

/// What the engine workspace's file covers, and the gate that keeps it honest.
const ENGINE_WORKSPACE_SCOPE_AND_REGENERATION_PROSE: &str = r"Scope is the whole workspace's dependency closure across every target, which is
a superset of what any one artifact links — the wheel ships this file entire
rather than a per-artifact subset, because over-attribution costs a longer file
and under-attribution is a term unmet.

**Generated — do not edit.** Regenerate with `cargo xtask
generate-third-party-notices`; the licence policy that decides what may appear
here lives in `deny.toml` and is enforced on every pull request. StreamLib's own
terms are in [`LICENSE`](LICENSE) and are not reproduced below.
";

/// The handlebars template `cargo about generate` renders — the licence
/// sections only; the preamble above them is written here, per target.
const CARGO_ABOUT_TEMPLATE_FILE_NAME: &str = "about.hbs";

/// The accepted-licence policy every project in this repo is generated against.
const CARGO_ABOUT_CONFIG_FILE_NAME: &str = "about.toml";

/// What makes a `packages/` directory an extension wheel: pip records this group
/// at install, and the engine reads it back when a process takes an engine role.
const EXTENSION_ENTRY_POINT_GROUP: &str = "streamlib.extensions";

/// The generator whose published-project list decides what the simple index
/// serves — a wheel it does not name is one pip cannot resolve.
const SIMPLE_INDEX_GENERATOR_RELATIVE_PATH: &str = "scripts/build_simple_index.py";

/// The crate whose build directory holds the extracted vendored C++ trees.
const SHADERC_VENDORING_CRATE_NAME: &str = "shaderc-sys";

/// The crate whose checkout carries libopus's own sources and `COPYING`.
const OPUSIC_VENDORING_CRATE_NAME: &str = "opusic-sys";

/// Which bullet of the appendix's roster a project belongs under.
///
/// Named separately from the notice source because the two answer different
/// questions — where the text is read from, and how the code got into the
/// binary — and because [`Self::ALL`] is what keeps the bullet list and the
/// check that nothing fell out of it from being two lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VendoredCppNoticeRoster {
    LinkedThroughShadercSys,
    LinkedThroughOpusicSys,
    CompiledByTheVulkanaliaVmaForksBuildScript,
    CompiledByTheEnginesBuildScript,
}

impl VendoredCppNoticeRoster {
    const ALL: [Self; 4] = [
        Self::LinkedThroughShadercSys,
        Self::LinkedThroughOpusicSys,
        Self::CompiledByTheVulkanaliaVmaForksBuildScript,
        Self::CompiledByTheEnginesBuildScript,
    ];

    /// How the bullet introduces the projects it then names.
    fn how_the_code_reaches_the_binary(self) -> &'static str {
        match self {
            Self::LinkedThroughShadercSys => {
                "Through the `shaderc-sys` crate, linked into `libshaderc_combined.a`"
            }
            Self::LinkedThroughOpusicSys => {
                "Through the `opusic-sys` crate, linked statically into the engine as \
                 `libopus.a`"
            }
            Self::CompiledByTheVulkanaliaVmaForksBuildScript => {
                "Checked into this repository, compiled by \
                 `vendor/tatolab-vulkanalia-vma/build.rs`"
            }
            Self::CompiledByTheEnginesBuildScript => {
                "Checked into this repository as headers, compiled into the engine by \
                 `runtime/streamlib-engine/build.rs`"
            }
        }
    }
}

/// Where a vendored C++ project's notice text is read from.
///
/// One shape per tree because they genuinely differ, not as a convenience: a
/// build-script crate in the registry checkout ships a licence file, the trees
/// the vulkanalia VMA fork vendors ship none, and `vendor/pipewire-headers/`
/// carries its own.
enum VendoredCppNoticeSource {
    /// A licence file inside a build-script crate's own registry checkout,
    /// reproduced whole. The path is relative to the crate root rather than to
    /// any one crate's layout, because the two that use this differ:
    /// `shaderc-sys` extracts its trees under `build/`, and `opusic-sys`
    /// carries libopus's `COPYING` at its root.
    RegistryCrateLicenseFile {
        vendoring_crate_name: &'static str,
        path_relative_to_crate_root: &'static str,
    },
    /// The comment block heading a header — the only place these projects state
    /// their copyright.
    LeadingCommentBlockOf {
        header_path_relative_to_workspace_root: &'static str,
    },
    /// A licence file checked into this repository's own `vendor/` tree,
    /// reproduced whole.
    VendoredLicenseFile {
        path_relative_to_workspace_root: &'static str,
    },
}

/// The two trees the notices are read out of, resolved once per run.
///
/// One argument rather than two adjacent `&Path`s: they are the same type, and
/// a transposition would compile clean and reproduce the wrong file's text.
struct VendoredCppSourceTrees {
    /// Registry checkout root per vendoring crate, keyed by crate name.
    /// Resolved once per run for each distinct crate the roster names.
    registry_crate_roots: BTreeMap<&'static str, PathBuf>,
    /// This repository's root, which `vendor/tatolab-vulkanalia-vma/` hangs off.
    workspace_root: PathBuf,
}

/// One C++ project compiled into the engine, with the notice that has to travel
/// with the binary.
struct VendoredCppProjectLinkedIntoTheEngine {
    /// The upstream project's own name, not the directory it lands in.
    display_name: &'static str,
    upstream_repository_url: &'static str,
    /// What the notice actually contains, for the section heading. glslang's is
    /// not a single licence: it is a manifest covering several, which is why
    /// it is 54 KB and the others are 11–23 KB.
    license_summary: &'static str,
    notice_source: VendoredCppNoticeSource,
    /// Which bullet of the roster this project is named under. Explicit
    /// rather than derived from `notice_source`, because where a notice is
    /// read from and how the code reached the binary are different questions
    /// — two crates now share one notice source and sit on different bullets.
    roster: VendoredCppNoticeRoster,
}

const VENDORED_CPP_PROJECTS: &[VendoredCppProjectLinkedIntoTheEngine] = &[
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "shaderc",
        upstream_repository_url: "https://github.com/google/shaderc",
        license_summary: "Apache-2.0",
        notice_source: VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name: SHADERC_VENDORING_CRATE_NAME,
            path_relative_to_crate_root: "build/shaderc/LICENSE",
        },
        roster: VendoredCppNoticeRoster::LinkedThroughShadercSys,
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "glslang",
        upstream_repository_url: "https://github.com/KhronosGroup/glslang",
        license_summary: "BSD-3-Clause, BSD-2-Clause, MIT, Apache-2.0, and GPL-3.0 WITH Bison-exception-2.2",
        // `LICENSE.txt`, where the other three are `LICENSE`. Spelled out per
        // project rather than globbed for exactly this reason: a glob hides the
        // asymmetry, and hiding it is how a rename becomes a dropped notice.
        notice_source: VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name: SHADERC_VENDORING_CRATE_NAME,
            path_relative_to_crate_root: "build/glslang/LICENSE.txt",
        },
        roster: VendoredCppNoticeRoster::LinkedThroughShadercSys,
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "SPIRV-Tools",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Tools",
        license_summary: "Apache-2.0",
        notice_source: VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name: SHADERC_VENDORING_CRATE_NAME,
            path_relative_to_crate_root: "build/spirv-tools/LICENSE",
        },
        roster: VendoredCppNoticeRoster::LinkedThroughShadercSys,
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "SPIRV-Headers",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Headers",
        license_summary: "MIT, with an Apache-2.0 carve-out the file names",
        notice_source: VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name: SHADERC_VENDORING_CRATE_NAME,
            path_relative_to_crate_root: "build/spirv-headers/LICENSE",
        },
        roster: VendoredCppNoticeRoster::LinkedThroughShadercSys,
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "VulkanMemoryAllocator",
        upstream_repository_url: "https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator",
        license_summary: "MIT",
        notice_source: VendoredCppNoticeSource::LeadingCommentBlockOf {
            header_path_relative_to_workspace_root: "vendor/tatolab-vulkanalia-vma/vendor/VulkanMemoryAllocator/include/vk_mem_alloc.h",
        },
        roster: VendoredCppNoticeRoster::CompiledByTheVulkanaliaVmaForksBuildScript,
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "Vulkan-Headers",
        upstream_repository_url: "https://github.com/KhronosGroup/Vulkan-Headers",
        // The header states the identifier rather than carrying the terms, so
        // the notice below is the copyright line and the Apache-2.0 text it
        // points at is the one reproduced in full under "Apache License 2.0".
        license_summary: "Apache-2.0, whose full text is reproduced above",
        notice_source: VendoredCppNoticeSource::LeadingCommentBlockOf {
            header_path_relative_to_workspace_root: "vendor/tatolab-vulkanalia-vma/vendor/Vulkan-Headers/include/vulkan/vulkan_core.h",
        },
        roster: VendoredCppNoticeRoster::CompiledByTheVulkanaliaVmaForksBuildScript,
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "PipeWire",
        upstream_repository_url: "https://gitlab.freedesktop.org/pipewire/pipewire",
        license_summary: "MIT",
        // Headers only — no PipeWire source is compiled and no PipeWire library
        // is linked. What ships inside the wheel is SPA's `static inline` pod
        // builders and parsers, compiled into
        // `runtime/streamlib-engine/src/linux/pipewire_audio_shim.c`, which is
        // why the terms travel with the binary all the same.
        notice_source: VendoredCppNoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/pipewire-headers/COPYING",
        },
        roster: VendoredCppNoticeRoster::CompiledByTheEnginesBuildScript,
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "libopus",
        upstream_repository_url: "https://gitlab.xiph.org/xiph/opus",
        license_summary: "BSD-3-Clause",
        // libopus's own `COPYING`, inside the sources `opusic-sys` bundles —
        // the same relationship shaderc's `build/shaderc/LICENSE` has to its
        // vendoring crate, rather than the crate's own root `LICENSE`, which
        // is opusic-sys's copy of the same text. Different layout, same
        // mechanism, which is why the path is relative to the crate root
        // rather than to any one crate's convention.
        notice_source: VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name: OPUSIC_VENDORING_CRATE_NAME,
            path_relative_to_crate_root: "opus/COPYING",
        },
        roster: VendoredCppNoticeRoster::LinkedThroughOpusicSys,
    },
];

/// Which project's notices a run generates.
///
/// The engine workspace and a standalone extension wheel differ in all three
/// things a run has to know — the manifest `cargo about` resolves, where the
/// file lands, and whether the vendored C++ appendix belongs on the end — so
/// they are one choice rather than three flags that could disagree.
pub enum NoticesGenerationTarget {
    /// The engine workspace: the wheel, the SDK crate and everything they link.
    EngineWorkspace,
    /// One standalone extension wheel, by its directory relative to the
    /// workspace root. Not a workspace member, so `cargo about` has to be
    /// pointed at its manifest and reads its own lockfile.
    ExtensionPackage { package_directory: PathBuf },
}

impl NoticesGenerationTarget {
    /// The engine workspace unless an extension package was named.
    pub fn for_extension_package_directory(package_directory: Option<&Path>) -> Self {
        match package_directory {
            None => Self::EngineWorkspace,
            Some(package_directory) => Self::ExtensionPackage {
                package_directory: package_directory.to_path_buf(),
            },
        }
    }

    /// The project root: the manifest to resolve, and where the file lands.
    fn project_root(&self, workspace_root: &Path) -> PathBuf {
        match self {
            Self::EngineWorkspace => workspace_root.to_path_buf(),
            Self::ExtensionPackage { package_directory } => workspace_root.join(package_directory),
        }
    }

    /// Everything above `## Overview`, which the template renders.
    fn preamble(&self) -> String {
        let scope_and_regeneration = match self {
            Self::EngineWorkspace => ENGINE_WORKSPACE_SCOPE_AND_REGENERATION_PROSE.to_owned(),
            Self::ExtensionPackage { package_directory } => format!(
                r"Scope is this wheel's own dependency closure across every target, which is a
superset of what the built artifact links — the wheel ships this file entire
rather than a per-artifact subset, because over-attribution costs a longer file
and under-attribution is a term unmet. This is an extension wheel: it links no
engine crate, so the `streamlib` wheel's own notices are neither reproduced nor
implied here.

**Generated — do not edit.** Regenerate with
`cargo xtask generate-third-party-notices --extension-package-directory {}`;
the accepted licences are the workspace's own `about.toml`, which this project
shares — a licence outside that list fails the regeneration itself rather than
landing here quietly. StreamLib's own terms are in [`LICENSE`](LICENSE) and are
not reproduced below.
",
                package_directory.display()
            ),
        };
        format!("{NOTICES_PREAMBLE_OPENING}\n{scope_and_regeneration}\n")
    }
}

/// Regenerate one project's notices file in that project's own root.
pub fn run(workspace_root: &Path, target: &NoticesGenerationTarget) -> Result<()> {
    let project_root = target.project_root(workspace_root);
    let mut notices = target.preamble();
    notices.push_str(&run_cargo_about_generate(workspace_root, &project_root)?);

    if let NoticesGenerationTarget::EngineWorkspace = target {
        let source_trees = VendoredCppSourceTrees {
            registry_crate_roots: locate_registry_crate_roots(workspace_root)?,
            workspace_root: workspace_root.to_path_buf(),
        };
        notices.push('\n');
        notices.push_str(&render_vendored_cpp_appendix(&source_trees)?);
    }

    let notices_path = project_root.join(THIRD_PARTY_NOTICES_FILE_NAME);
    std::fs::write(&notices_path, notices)
        .with_context(|| format!("writing {}", notices_path.display()))?;

    match target {
        NoticesGenerationTarget::EngineWorkspace => tracing::info!(
            "wrote {} ({} vendored C++ projects appended)",
            notices_path.display(),
            VENDORED_CPP_PROJECTS.len()
        ),
        NoticesGenerationTarget::ExtensionPackage { .. } => {
            tracing::info!("wrote {}", notices_path.display())
        }
    }
    Ok(())
}

/// Render the Rust closure's notices by shelling out to `cargo about`.
///
/// Run from the workspace root whatever the target, so the shared `about.toml`
/// and `about.hbs` resolve the same way for every project — an extension's
/// accepted-licence list is the engine's, deliberately, because one repo takes
/// one licensing posture. `--config` is passed rather than left to default,
/// which would look for an `about.toml` beside the extension's own manifest.
///
/// Its warnings are surfaced rather than swallowed: "unable to synthesize a
/// license expression for X" is this tool's way of saying a crate reached the
/// artifact with no notice, which is the one thing this file exists to prevent.
/// The workspace's own crates warn on every run and are the expected noise —
/// they carry `license-file`, and a file titled *third-party* notices is not
/// where our own terms belong.
fn run_cargo_about_generate(workspace_root: &Path, project_root: &Path) -> Result<String> {
    let output = std::process::Command::new("cargo")
        // `--locked` not only for symmetry: this runs before the `cargo metadata
        // --locked` behind the vendored lookup, so without it a stale lock is
        // rewritten here and that guard then passes against the rewrite —
        // notices describing a graph the commit does not contain.
        //
        // `--all-features` to read the same graph `deny.toml` validates with
        // `all-features = true`. Without it the gate vets five crates behind
        // optional features that the notices never reproduce, and two configs
        // claiming one licence set would be reading two graphs. Safe despite the
        // repo-wide `--all-features` ban for the same reason it is safe there:
        // cargo-about compiles nothing, so no build script regenerates the
        // vendored VMA bindings.
        //
        // No `--workspace`: the root manifest is virtual, so every member is
        // already in scope and the flag leaves the output byte-identical.
        .args(["about", "generate", "--locked", "--all-features"])
        .arg("--config")
        .arg(CARGO_ABOUT_CONFIG_FILE_NAME)
        .arg("--manifest-path")
        .arg(project_root.join("Cargo.toml"))
        .arg(CARGO_ABOUT_TEMPLATE_FILE_NAME)
        .current_dir(workspace_root)
        .output()
        .context("spawning `cargo about generate` — `cargo install cargo-about` if missing")?;

    let diagnostics = String::from_utf8_lossy(&output.stderr);
    anyhow::ensure!(
        output.status.success(),
        "`cargo about generate` failed ({}): {}",
        output.status,
        diagnostics.trim(),
    );
    if !diagnostics.trim().is_empty() {
        tracing::warn!("cargo about generate:\n{}", diagnostics.trim());
    }

    String::from_utf8(output.stdout).context("`cargo about generate` emitted non-UTF-8")
}

/// Find every vendoring crate's registry checkout through `cargo metadata`.
///
/// Resolved rather than hard-coded: the sources live in the registry checkout,
/// whose path carries a version and a registry hash that both move. Driven off
/// the roster itself, so a project added with a new vendoring crate resolves
/// without a second list to keep in step.
fn locate_registry_crate_roots(workspace_root: &Path) -> Result<BTreeMap<&'static str, PathBuf>> {
    let metadata = crate::run_cargo_metadata_resolve_document(workspace_root)?;
    let mut roots = BTreeMap::new();
    for project in VENDORED_CPP_PROJECTS {
        let VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name,
            ..
        } = project.notice_source
        else {
            continue;
        };
        if roots.contains_key(vendoring_crate_name) {
            continue;
        }
        let crate_root = registry_crate_root_in(&metadata, vendoring_crate_name)?;
        anyhow::ensure!(
            crate_root.is_dir(),
            "{} does not exist — run `cargo fetch` first",
            crate_root.display()
        );
        roots.insert(vendoring_crate_name, crate_root);
    }
    Ok(roots)
}

/// The registry checkout root a `cargo metadata` document points a vendoring
/// crate at.
///
/// Split from the spawn so both refusals are reachable from a test. They are
/// not hypothetical: renaming, replacing or feature-gating a vendoring crate
/// silently drops its notices, and two copies of it in one graph means the
/// appendix covers whichever the iterator saw first.
fn registry_crate_root_in(
    metadata: &serde_json::Value,
    vendoring_crate_name: &str,
) -> Result<PathBuf> {
    let manifest_paths: Vec<&str> = metadata["packages"]
        .as_array()
        .context("cargo metadata has no packages array")?
        .iter()
        .filter(|package| package["name"].as_str() == Some(vendoring_crate_name))
        .filter_map(|package| package["manifest_path"].as_str())
        .collect();

    let [manifest_path] = manifest_paths.as_slice() else {
        anyhow::bail!(
            "expected exactly one {vendoring_crate_name} in the graph, found {}. {}",
            manifest_paths.len(),
            if manifest_paths.is_empty() {
                "Its vendored C++ notices would be dropped entirely — update the vendoring \
                 crate name on the roster if the code moved to another crate"
            } else {
                "The appendix would cover whichever copy came first, and say nothing about \
                 the rest"
            }
        );
    };

    Ok(Path::new(manifest_path)
        .parent()
        .with_context(|| format!("{manifest_path} has no parent directory"))?
        .to_path_buf())
}

/// Render the appended half: one section per vendored C++ project.
///
/// A missing or unreadable notice is an error, never an omitted section. The
/// failure mode this guards is a dependency bump or a re-vendor that renames or
/// moves one of the trees — silently shipping the binary without its terms is
/// the one outcome worse than a red build.
fn render_vendored_cpp_appendix(source_trees: &VendoredCppSourceTrees) -> Result<String> {
    let mut appendix = String::new();
    // The two rosters come off the table, and no count is stated at all. This
    // paragraph ships inside a legal notice: a seventh project must not be able
    // to leave it enumerating six with every test still green, and a number
    // nobody wrote down is a number that cannot go stale.
    write!(
        appendix,
        "## Vendored C++ sources\n\
         \n\
         The projects below are compiled into the engine from vendored sources rather than\n\
         linked as Cargo packages, so none of them appears in the resolve graph `cargo about`\n\
         walks — and every one of them ships inside the wheel. These sections are appended by\n\
         `cargo xtask generate-third-party-notices`.\n\
         \n\
         {}",
        vendored_cpp_roster_bullets(),
    )?;

    for project in VENDORED_CPP_PROJECTS {
        let notice = read_vendored_cpp_notice(project, source_trees)?;
        write!(
            appendix,
            "\n### {} ({})\n\nUpstream: <{}>\n\n````text\n{}\n````\n",
            project.display_name,
            project.license_summary,
            project.upstream_repository_url,
            notice.trim_end(),
        )?;
    }

    Ok(appendix)
}

/// One bullet per roster, each naming the projects that reach the binary that
/// way. Rendered off [`VendoredCppNoticeRoster::ALL`] rather than written out,
/// so an eighth project cannot land in a table the prose above it never
/// mentions.
fn vendored_cpp_roster_bullets() -> String {
    VendoredCppNoticeRoster::ALL
        .iter()
        .map(|roster| {
            format!(
                "- {}: {}\n",
                roster.how_the_code_reaches_the_binary(),
                joined_vendored_cpp_project_display_names(*roster)
            )
        })
        .collect()
}

/// The display names of every project in one roster, as an English list the
/// surrounding sentence can take.
fn joined_vendored_cpp_project_display_names(roster: VendoredCppNoticeRoster) -> String {
    let names: Vec<&str> = VENDORED_CPP_PROJECTS
        .iter()
        .filter(|project| project.roster == roster)
        .map(|project| project.display_name)
        .collect();

    match names.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, leading)) => format!("{} and {last}", leading.join(", ")),
    }
}

/// Read one project's notice text from whichever tree holds it.
fn read_vendored_cpp_notice(
    project: &VendoredCppProjectLinkedIntoTheEngine,
    source_trees: &VendoredCppSourceTrees,
) -> Result<String> {
    let (path, notice) = match project.notice_source {
        VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name,
            path_relative_to_crate_root,
        } => {
            let crate_root = source_trees
                .registry_crate_roots
                .get(vendoring_crate_name)
                .with_context(|| {
                    format!(
                        "no registry checkout was resolved for {vendoring_crate_name}, which \
                         the {} notice is read out of",
                        project.display_name
                    )
                })?;
            let path = crate_root.join(path_relative_to_crate_root);
            let text = read_notice_file(&path, project.display_name)?;
            (path, text)
        }
        VendoredCppNoticeSource::LeadingCommentBlockOf {
            header_path_relative_to_workspace_root,
        } => {
            let path = source_trees
                .workspace_root
                .join(header_path_relative_to_workspace_root);
            let source = read_notice_file(&path, project.display_name)?;
            let text = leading_comment_block_notice(&source).with_context(|| {
                format!(
                    "reading the {} notice at {}",
                    project.display_name,
                    path.display()
                )
            })?;
            (path, text)
        }
        VendoredCppNoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root,
        } => {
            let path = source_trees
                .workspace_root
                .join(path_relative_to_workspace_root);
            let text = read_notice_file(&path, project.display_name)?;
            (path, text)
        }
    };

    anyhow::ensure!(
        notice.contains("Copyright"),
        "the {} notice at {} carries no copyright line — reproducing it would discharge nothing",
        project.display_name,
        path.display()
    );
    Ok(notice)
}

fn read_notice_file(path: &Path, display_name: &str) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading the {display_name} notice at {} — a bump or re-vendor that moved it would \
             otherwise drop the notice silently",
            path.display()
        )
    })
}

/// The comment block heading a C or C++ source file, with its markers stripped.
///
/// Not a C parser: it takes the first `//` run or `/* … */` block, skipping
/// blank lines and preprocessor directives, which is where both of these trees
/// state their copyright — VulkanMemoryAllocator opens the file with it, and
/// Vulkan-Headers puts it after the include guard. Each branch strips only its
/// own markers, so a `*` that is bullet text rather than a block-comment rail
/// survives the line-comment shape.
fn leading_comment_block_notice(source: &str) -> Result<String> {
    let mut lines = source
        .lines()
        .skip_while(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
        .peekable();

    let first = *lines
        .peek()
        .context("the file holds no comment block at all")?;

    let notice_lines: Vec<&str> = if first.trim_start().starts_with("//") {
        lines
            .take_while(|line| line.trim_start().starts_with("//"))
            .map(|line| line.trim().trim_start_matches("//").trim())
            .collect()
    } else if first.trim_start().starts_with("/*") {
        let mut collected = Vec::new();
        let mut block_comment_is_closed = false;
        for line in lines {
            block_comment_is_closed = line.contains("*/");
            collected.push(
                line.trim()
                    .trim_start_matches("/*")
                    .trim_end_matches("*/")
                    .trim_start_matches('*')
                    .trim(),
            );
            if block_comment_is_closed {
                break;
            }
        }
        anyhow::ensure!(
            block_comment_is_closed,
            "the leading block comment is never closed"
        );
        collected
    } else {
        anyhow::bail!("the first non-directive line is not a comment: {first:?}");
    };

    Ok(notice_lines.join("\n").trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// The family idiom for the workspace root in a gate's tests: free, and it
    /// needs neither cargo on PATH nor the package lock.
    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ always has a workspace root above it")
            .to_path_buf()
    }

    /// A stand-in for `<shaderc-sys>/build/` holding every licence file the
    /// shaderc-side projects declare, each naming the project it covers.
    ///
    /// Paired with the real workspace root, so the comment-block projects read
    /// the actual vendored headers — `check-vendored-vulkanalia` hashes those
    /// trees, so a re-vendor that drops a copyright line fails here.
    fn vendored_cpp_source_trees_fixture() -> (TempDir, VendoredCppSourceTrees) {
        let fixture = TempDir::new().expect("temp dir");
        let mut registry_crate_roots = BTreeMap::new();
        for project in VENDORED_CPP_PROJECTS {
            let VendoredCppNoticeSource::RegistryCrateLicenseFile {
                vendoring_crate_name,
                path_relative_to_crate_root,
            } = project.notice_source
            else {
                continue;
            };
            // One checkout per vendoring crate, so a path that is right for
            // one crate's layout and wrong for another's fails here.
            let crate_root = fixture.path().join(vendoring_crate_name);
            registry_crate_roots.insert(vendoring_crate_name, crate_root.clone());
            let path = crate_root.join(path_relative_to_crate_root);
            fs::create_dir_all(path.parent().expect("a licence file sits in a directory"))
                .expect("fixture dir");
            fs::write(
                &path,
                format!("Copyright (c) terms covering {}\n", project.display_name),
            )
            .expect("fixture licence file");
        }
        let source_trees = VendoredCppSourceTrees {
            registry_crate_roots,
            workspace_root: workspace_root(),
        };
        (fixture, source_trees)
    }

    #[test]
    fn the_appendix_reproduces_every_vendored_project_verbatim() {
        let (_fixture, source_trees) = vendored_cpp_source_trees_fixture();
        let appendix = render_vendored_cpp_appendix(&source_trees).expect("render");

        for project in VENDORED_CPP_PROJECTS {
            assert!(
                appendix.contains(&format!("### {} (", project.display_name)),
                "{} has no section",
                project.display_name
            );
            assert!(
                appendix.contains(project.upstream_repository_url),
                "{} has no upstream link",
                project.display_name
            );
        }
        assert!(appendix.contains("Advanced Micro Devices"));
        assert!(appendix.contains("The Khronos Group Inc"));
        assert!(appendix.contains("Wim Taymans"));
    }

    #[test]
    fn a_moved_licence_file_fails_the_render_naming_the_project() {
        let (fixture, source_trees) = vendored_cpp_source_trees_fixture();
        let moved = VENDORED_CPP_PROJECTS
            .iter()
            .find(|project| project.display_name == "glslang")
            .expect("glslang is one of the vendored projects");
        let VendoredCppNoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name,
            path_relative_to_crate_root,
        } = moved.notice_source
        else {
            panic!("glslang's notice comes from a registry crate's licence file")
        };
        fs::remove_file(
            fixture
                .path()
                .join(vendoring_crate_name)
                .join(path_relative_to_crate_root),
        )
        .expect("remove");

        let failure = render_vendored_cpp_appendix(&source_trees)
            .expect_err("a missing licence file must not render as an omitted section");
        let reported = format!("{failure:#}");
        assert!(
            reported.contains("glslang"),
            "unhelpful failure: {reported}"
        );
        assert!(
            reported.contains(path_relative_to_crate_root),
            "unhelpful failure: {reported}"
        );
    }

    /// The appendix's opening paragraph ships inside a legal notice, and its
    /// two rosters are derived. A third notice source added later would render
    /// its projects into sections while leaving them out of both rosters —
    /// which is the paragraph claiming a coverage it no longer has.
    #[test]
    fn the_appendix_rosters_between_them_name_every_project_in_the_table() {
        let (_fixture, source_trees) = vendored_cpp_source_trees_fixture();
        let appendix = render_vendored_cpp_appendix(&source_trees).expect("render");

        let rosters = VendoredCppNoticeRoster::ALL.map(joined_vendored_cpp_project_display_names);
        for roster in &rosters {
            assert!(
                !roster.is_empty(),
                "a roster emptied, leaving prose dangling"
            );
            assert!(appendix.contains(roster.as_str()));
        }
        for project in VENDORED_CPP_PROJECTS {
            assert!(
                rosters
                    .iter()
                    .any(|roster| roster.contains(project.display_name)),
                "{} has a section but appears in neither roster",
                project.display_name
            );
        }
    }

    #[test]
    fn glslang_is_the_one_project_whose_licence_file_is_not_named_license() {
        // Not trivia: it is the whole reason the paths are spelled out instead
        // of globbed, and a future reader deleting the "redundant" extension is
        // exactly the edit this locks.
        let named_license_txt: Vec<&str> = VENDORED_CPP_PROJECTS
            .iter()
            .filter(|project| {
                matches!(
                    project.notice_source,
                    VendoredCppNoticeSource::RegistryCrateLicenseFile {
                        path_relative_to_crate_root: path,
                        ..
                    } if path.ends_with("LICENSE.txt")
                )
            })
            .map(|project| project.display_name)
            .collect();
        assert_eq!(named_license_txt, ["glslang"]);
    }

    #[test]
    fn a_line_comment_run_is_taken_whole() {
        let notice = leading_comment_block_notice(
            "// Copyright (c) 2017-2025 Someone\n//\n// Permission is granted.\n\n#ifndef GUARD\n",
        )
        .expect("extract");
        assert_eq!(
            notice,
            "Copyright (c) 2017-2025 Someone\n\nPermission is granted."
        );
    }

    #[test]
    fn an_include_guard_does_not_hide_the_block_comment_behind_it() {
        let notice = leading_comment_block_notice(
            "#ifndef GUARD\n#define GUARD 1\n\n/*\n** Copyright 2015-2025 Them\n**\n\
             ** SPDX-License-Identifier: Apache-2.0\n*/\n\n/*\n** Unrelated prose.\n*/\n",
        )
        .expect("extract");
        assert_eq!(
            notice,
            "Copyright 2015-2025 Them\n\nSPDX-License-Identifier: Apache-2.0"
        );
        assert!(!notice.contains("Unrelated prose"));
    }

    #[test]
    fn a_leading_block_with_no_copyright_is_refused_rather_than_reproduced() {
        let project = VendoredCppProjectLinkedIntoTheEngine {
            display_name: "Nameless",
            upstream_repository_url: "https://example.invalid",
            license_summary: "none",
            notice_source: VendoredCppNoticeSource::LeadingCommentBlockOf {
                header_path_relative_to_workspace_root: "header.h",
            },
            roster: VendoredCppNoticeRoster::CompiledByTheVulkanaliaVmaForksBuildScript,
        };
        let workspace = TempDir::new().expect("temp dir");
        fs::write(workspace.path().join("header.h"), "// just a description\n").expect("write");

        let source_trees = VendoredCppSourceTrees {
            registry_crate_roots: BTreeMap::new(),
            workspace_root: workspace.path().to_path_buf(),
        };

        let failure = read_vendored_cpp_notice(&project, &source_trees)
            .expect_err("a block with no copyright discharges nothing");
        assert!(
            format!("{failure:#}").contains("no copyright line"),
            "unhelpful failure: {failure:#}"
        );
    }

    #[test]
    fn a_graph_without_the_vendoring_crate_names_what_would_be_dropped() {
        let metadata = serde_json::json!({ "packages": [{ "name": "serde" }] });
        let failure = registry_crate_root_in(&metadata, SHADERC_VENDORING_CRATE_NAME)
            .expect_err("no shaderc-sys means four notices vanish");
        let reported = format!("{failure:#}");
        assert!(
            reported.contains("found 0"),
            "unhelpful failure: {reported}"
        );
        assert!(
            reported.contains("dropped entirely"),
            "unhelpful failure: {reported}"
        );
    }

    #[test]
    fn two_copies_of_the_vendoring_crate_are_refused_rather_than_guessed_between() {
        let metadata = serde_json::json!({
            "packages": [
                { "name": SHADERC_VENDORING_CRATE_NAME, "manifest_path": "/a/Cargo.toml" },
                { "name": SHADERC_VENDORING_CRATE_NAME, "manifest_path": "/b/Cargo.toml" },
            ]
        });
        let failure = registry_crate_root_in(&metadata, SHADERC_VENDORING_CRATE_NAME)
            .expect_err("two copies means the appendix silently covers one");
        assert!(
            format!("{failure:#}").contains("found 2"),
            "unhelpful failure: {failure:#}"
        );
    }

    #[test]
    fn a_vendoring_crates_own_checkout_root_is_what_gets_resolved() {
        let metadata = serde_json::json!({
            "packages": [
                {
                    "name": SHADERC_VENDORING_CRATE_NAME,
                    "manifest_path": "/registry/shaderc-sys-0.10.1/Cargo.toml",
                },
                {
                    "name": OPUSIC_VENDORING_CRATE_NAME,
                    "manifest_path": "/registry/opusic-sys-0.7.5/Cargo.toml",
                },
            ]
        });
        assert_eq!(
            registry_crate_root_in(&metadata, SHADERC_VENDORING_CRATE_NAME).expect("resolve"),
            PathBuf::from("/registry/shaderc-sys-0.10.1")
        );
        assert_eq!(
            registry_crate_root_in(&metadata, OPUSIC_VENDORING_CRATE_NAME).expect("resolve"),
            PathBuf::from("/registry/opusic-sys-0.7.5")
        );
    }

    /// The two vendoring crates lay their notices out differently — one under
    /// `build/`, one at the crate root — and the roster is what says so. A
    /// path written for the wrong crate's convention reads nothing, so the
    /// prefixes are locked rather than left to a reviewer to notice.
    #[test]
    fn each_vendoring_crates_notice_paths_follow_that_crates_own_layout() {
        for project in VENDORED_CPP_PROJECTS {
            let VendoredCppNoticeSource::RegistryCrateLicenseFile {
                vendoring_crate_name,
                path_relative_to_crate_root,
            } = project.notice_source
            else {
                continue;
            };
            match vendoring_crate_name {
                SHADERC_VENDORING_CRATE_NAME => assert!(
                    path_relative_to_crate_root.starts_with("build/"),
                    "{} extracts its trees under build/, got {path_relative_to_crate_root}",
                    project.display_name
                ),
                OPUSIC_VENDORING_CRATE_NAME => assert_eq!(
                    path_relative_to_crate_root, "opus/COPYING",
                    "{} reads libopus's own COPYING inside the bundled sources",
                    project.display_name
                ),
                other => panic!("{other} has no stated notice layout"),
            }
        }
    }

    /// The wheel and the SDK crate both reach the notices by symlink, which a
    /// rename or a `git mv` can break without touching a line of this file.
    /// `read_to_string` follows symlinks, so a dangling one fails here rather
    /// than at release time.
    #[test]
    fn every_artifact_path_that_ships_the_notices_resolves_to_readable_text() {
        let workspace_root = workspace_root();
        let wheel_dir = workspace_root.join("sdk/streamlib-python-wheel");
        let pyproject: toml::Value =
            toml::from_str(&fs::read_to_string(wheel_dir.join("pyproject.toml")).expect("read"))
                .expect("parse pyproject.toml");

        let declared = pyproject["project"]["license-files"]
            .as_array()
            .expect("the wheel declares PEP 639 license-files");
        assert!(
            declared
                .iter()
                .any(|entry| entry.as_str() == Some(THIRD_PARTY_NOTICES_FILE_NAME)),
            "the wheel ships third-party code; {THIRD_PARTY_NOTICES_FILE_NAME} must be in \
             license-files"
        );

        let mut paths: Vec<PathBuf> = declared
            .iter()
            .map(|entry| wheel_dir.join(entry.as_str().expect("license-files entries are strings")))
            .collect();
        // `cargo package` dereferences a symlink into the archive, so the
        // published `streamlib` crate carries the same bytes beside its source.
        paths.push(
            workspace_root
                .join("sdk/streamlib-sdk")
                .join(THIRD_PARTY_NOTICES_FILE_NAME),
        );

        for path in paths {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|failure| panic!("{}: {failure}", path.display()));
            assert!(!contents.trim().is_empty(), "{} is empty", path.display());
        }
    }

    /// The preamble moved out of `about.hbs` and into this file. It is the one
    /// part of a generated notices file a human wrote, so a stray edit here
    /// would rewrite the opening of a legal notice on the next regeneration
    /// with nothing to catch it.
    #[test]
    fn the_committed_engine_notices_open_with_the_preamble_this_file_renders() {
        let committed =
            fs::read_to_string(workspace_root().join(THIRD_PARTY_NOTICES_FILE_NAME)).expect("read");

        assert!(
            committed.starts_with(&NoticesGenerationTarget::EngineWorkspace.preamble()),
            "the committed notices no longer open with the rendered preamble"
        );
    }

    /// An extension's file has to say what it covers and how to reproduce it,
    /// and must not inherit the engine's claims: its closure is not the
    /// workspace's, and `deny.toml` does not gate a non-member on any PR.
    #[test]
    fn an_extension_preamble_names_its_own_package_and_claims_nothing_it_cannot() {
        let preamble = NoticesGenerationTarget::ExtensionPackage {
            package_directory: PathBuf::from("packages/streamlib-webrtc"),
        }
        .preamble();

        assert!(preamble.contains(
            "--extension-package-directory packages/streamlib-webrtc"
        ));
        assert!(!preamble.contains("deny.toml"));
        assert!(!preamble.contains("the whole workspace's dependency closure"));
    }

    /// Every extension wheel discharges its own notice obligation, and the
    /// index that serves it knows its name.
    ///
    /// Discovered from the entry-point group rather than listed, so a third
    /// extension is covered the day its `pyproject.toml` lands — and the two
    /// halves are asserted together because the failure they guard is one
    /// thing: a wheel that ships without its notices, or ships and is
    /// unreachable because the index was never told about it.
    #[test]
    fn every_extension_wheel_ships_its_notices_and_is_named_by_the_simple_index() {
        let workspace_root = workspace_root();
        let published_project_names =
            fs::read_to_string(workspace_root.join(SIMPLE_INDEX_GENERATOR_RELATIVE_PATH))
                .expect("read the simple index generator");

        let mut extensions_checked = 0;
        for entry in fs::read_dir(workspace_root.join("packages")).expect("read packages/") {
            let package_directory = entry.expect("read a packages/ entry").path();
            let pyproject_path = package_directory.join("pyproject.toml");
            let Ok(pyproject_source) = fs::read_to_string(&pyproject_path) else {
                continue;
            };
            if !pyproject_source.contains(EXTENSION_ENTRY_POINT_GROUP) {
                continue;
            }
            extensions_checked += 1;

            let pyproject: toml::Value = toml::from_str(&pyproject_source)
                .unwrap_or_else(|failure| panic!("{}: {failure}", pyproject_path.display()));
            let declared = pyproject["project"]["license-files"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("{} declares no PEP 639 license-files", pyproject_path.display())
                });
            assert!(
                declared
                    .iter()
                    .any(|entry| entry.as_str() == Some(THIRD_PARTY_NOTICES_FILE_NAME)),
                "{} ships third-party code; {THIRD_PARTY_NOTICES_FILE_NAME} must be in \
                 license-files",
                pyproject_path.display()
            );

            for entry in declared {
                let path = package_directory
                    .join(entry.as_str().expect("license-files entries are strings"));
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|failure| panic!("{}: {failure}", path.display()));
                assert!(!contents.trim().is_empty(), "{} is empty", path.display());
            }

            let distribution_name = pyproject["project"]["name"]
                .as_str()
                .expect("a distribution name");
            assert!(
                published_project_names.contains(&format!("\"{distribution_name}\"")),
                "{distribution_name} is released but {SIMPLE_INDEX_GENERATOR_RELATIVE_PATH} does \
                 not publish it — the wheel would be built, attached and unreachable by pip"
            );
        }

        assert!(
            extensions_checked > 0,
            "no extension wheel was found under packages/ — the discovery rule moved out from \
             under this test, which would let an extension ship with no notices unnoticed"
        );
    }
}
