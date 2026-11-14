// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Regenerates a project's `THIRD-PARTY-NOTICES.md`.
//!
//! Two projects' shapes, one command. The engine workspace's file is the wheel's
//! and the SDK crate's; a standalone extension wheel under `packages/` is not a
//! workspace member and links no engine crate, so it owes a file of its own over
//! its own closure. [`NoticesGenerationTarget`] is that choice, and it decides
//! the manifest, the destination and the appendix together.
//!
//! The preamble is written here rather than in `about.hbs`, because what a file
//! covers and how to regenerate it differ per project while the licence sections
//! below them do not — one template, one prose block per target.
//!
//! Two halves that no single tool covers. `cargo about generate` walks the
//! resolve graph and reproduces each crate's licence text; the projects
//! appended here owe terms it cannot reach, because they are no package in that
//! graph at all. Every one of them ships inside the wheel
//! [`WheelCarryingTheNotice`] names, and only that wheel's file appends it.
//! [`PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE`] is the roster; this doc
//! does not repeat it, because a census in prose goes stale the moment the table
//! grows.
//!
//! The notice source is an enum because the trees genuinely differ.
//! `opusic-sys` and `libgit2-sys` carry their C sources in their own registry
//! checkouts, each with a licence file. The trees
//! `vendor/tatolab-vulkanalia-vma/build.rs` compiles `wrapper.cpp` against
//! carry no licence file at all — their copyright line exists only in the
//! comment block heading a header. The PipeWire/SPA headers, and the projects
//! compiled into the prebuilt libraries a wheel carries, are checked in here
//! with their own licence files.
//!
//! Not a CI gate, and deliberately so. This needs `cargo-about` installed and
//! reaches the network for crates that ship no licence file of their own. The
//! check that runs on every pull request is `cargo deny check licenses` —
//! a generated file rots, and the gate is what stops a licence outside the
//! allowlist landing quietly in between regenerations.
//!
//! `cargo about`'s own `--fail` is not the backstop and is not passed: the
//! workspace's seventeen publishable crates carry `license-file` rather than an
//! SPDX `license`, so it would fail every run. A dependency whose expression
//! cannot be synthesised is warned about here and omitted from the output; what
//! refuses it is `cargo deny check licenses`, which reads the same crate as
//! `unlicensed`.

use crate::extension_build_script_fetched_notices::render_the_appendix_of_sources_the_build_script_fetches;
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

/// The same, for one extension wheel, stored the same way as its twin above so
/// that finding one finds the other — they share sentences a licence review
/// would expect to move together. Substituted rather than formatted because a
/// `format!` string cannot be a `const`.
const EXTENSION_SCOPE_AND_REGENERATION_PROSE_TEMPLATE: &str = r"Scope is this wheel's own dependency closure across every target, which is a
superset of what the built artifact links — the wheel ships this file entire
rather than a per-artifact subset, because over-attribution costs a longer file
and under-attribution is a term unmet. This is an extension wheel: it links no
engine crate, so the `streamlib` wheel's own notices are neither reproduced nor
implied here.

**Generated — do not edit.** Regenerate with
`cargo xtask generate-third-party-notices --extension-package-directory {package_directory}`;
the accepted licences are the workspace's own `about.toml`, which this project
shares — a licence outside that list fails the regeneration itself rather than
landing here quietly. StreamLib's own terms are in [`LICENSE`](LICENSE) and are
not reproduced below.
";

/// What [`EXTENSION_SCOPE_AND_REGENERATION_PROSE_TEMPLATE`] carries in place of
/// the package this run was pointed at.
const EXTENSION_PACKAGE_DIRECTORY_PLACEHOLDER: &str = "{package_directory}";

/// The handlebars template `cargo about generate` renders — the licence
/// sections only; the preamble above them is written here, per target.
const CARGO_ABOUT_TEMPLATE_FILE_NAME: &str = "about.hbs";

/// The accepted-licence policy every project in this repo is generated against.
const CARGO_ABOUT_CONFIG_FILE_NAME: &str = "about.toml";

/// The crate whose checkout carries libopus's own sources and `COPYING`.
const OPUSIC_VENDORING_CRATE_NAME: &str = "opusic-sys";

/// The crate whose checkout carries libgit2's own sources and `COPYING`.
const LIBGIT2_VENDORING_CRATE_NAME: &str = "libgit2-sys";

/// The extension wheel that links libopus.
const OPUS_EXTENSION_PACKAGE_DIRECTORY: &str = "packages/tatolab-opus";

/// Which wheel a project's code ships inside, and so whose notices file appends
/// its terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WheelCarryingTheNotice {
    /// The engine wheel, whose file the SDK crate ships too.
    TheEngineWheel,
    /// The extension wheel in this repository-relative package directory.
    TheExtensionWheelIn(&'static str),
}

/// Which bullet of the appendix's roster a project belongs under.
///
/// Named separately from the notice source because the two answer different
/// questions — where the text is read from, and how the code got into the
/// binary — and because [`Self::ALL`] is what keeps the bullet list and the
/// check that nothing fell out of it from being two lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeRosterCargoAboutCannotProduce {
    CarriedPrebuiltInTheSlangCompiler,
    LinkedThroughOpusicSys,
    LinkedThroughLibgit2Sys,
    CompiledByTheVulkanaliaVmaForksBuildScript,
    CompiledByTheEnginesBuildScript,
    CarriedPrebuiltInTheBundledVulkanLoader,
    CarriedPrebuiltByTheMacOSWheel,
    CarriedPrebuiltByTheLinuxWheel,
    CarriedInTheRuntimeUnitForNewProjects,
}

impl NoticeRosterCargoAboutCannotProduce {
    const ALL: [Self; 9] = [
        Self::CarriedPrebuiltInTheSlangCompiler,
        Self::LinkedThroughOpusicSys,
        Self::LinkedThroughLibgit2Sys,
        Self::CompiledByTheVulkanaliaVmaForksBuildScript,
        Self::CompiledByTheEnginesBuildScript,
        Self::CarriedPrebuiltInTheBundledVulkanLoader,
        Self::CarriedPrebuiltByTheMacOSWheel,
        Self::CarriedPrebuiltByTheLinuxWheel,
        Self::CarriedInTheRuntimeUnitForNewProjects,
    ];

    /// How the bullet introduces the projects it then names.
    fn how_the_code_reaches_the_binary(self) -> &'static str {
        match self {
            Self::CarriedPrebuiltInTheSlangCompiler => {
                "In the macOS and Linux wheels, compiled into the Slang shader compiler \
                 libraries each carries beside the engine"
            }
            Self::LinkedThroughOpusicSys => {
                "Through the `opusic-sys` crate, linked statically into the native module as \
                 `libopus.a`"
            }
            Self::LinkedThroughLibgit2Sys => {
                "Through the `libgit2-sys` crate, compiled from its bundled sources and linked \
                 statically into `tatolabd`, which keeps each project's history with it"
            }
            Self::CompiledByTheVulkanaliaVmaForksBuildScript => {
                "Checked into this repository, compiled by \
                 `vendor/tatolab-vulkanalia-vma/build.rs`"
            }
            Self::CompiledByTheEnginesBuildScript => {
                "Checked into this repository as headers, compiled into the engine by \
                 `runtime/streamlib-engine/build.rs`"
            }
            Self::CarriedPrebuiltInTheBundledVulkanLoader => {
                "In the macOS and Linux wheels, compiled into the Vulkan loader library \
                 each carries beside the engine"
            }
            Self::CarriedPrebuiltByTheMacOSWheel => {
                "In the macOS wheel only, compiled into the MoltenVK library it carries \
                 beside the engine (SPIRV-Tools, SPIRV-Headers and Vulkan-Headers reach \
                 MoltenVK too, under the notices above)"
            }
            Self::CarriedPrebuiltByTheLinuxWheel => {
                "In the Linux wheel only, compiled into the lavapipe library it carries \
                 beside the engine (SPIRV-Headers and Vulkan-Headers reach lavapipe too, \
                 under the notices above)"
            }
            Self::CarriedInTheRuntimeUnitForNewProjects => {
                "In the runtime unit beside the engine, as the wheels and the CPython build a \
                 new project installs from it with no network, each archive carrying these \
                 licence files unmodified"
            }
        }
    }
}

/// Where one project's notice text is read from.
///
/// One shape per tree because they genuinely differ, not as a convenience: a
/// build-script crate in the registry checkout ships a licence file, the trees
/// the vulkanalia VMA fork vendors ship none, and `vendor/pipewire-headers/`
/// carries its own.
enum NoticeSource {
    /// A licence file inside a build-script crate's own registry checkout,
    /// reproduced whole. The path is relative to the crate root rather than to
    /// any one crate's layout, so each crate's own convention is spelled where
    /// its projects are listed.
    RegistryCrateLicenseFile {
        vendoring_crate_name: &'static str,
        path_relative_to_crate_root: &'static str,
    },
    /// The comment block heading a header — the only place these projects state
    /// their copyright.
    LeadingCommentBlockOf {
        header_path_relative_to_workspace_root: &'static str,
    },
    /// A notice file checked into this repository's own `vendor/` tree,
    /// reproduced whole.
    VendoredLicenseFile {
        path_relative_to_workspace_root: &'static str,
    },
}

/// The two trees the notices are read out of, resolved once per run.
///
/// One argument rather than two adjacent `&Path`s: they are the same type, and
/// a transposition would compile clean and reproduce the wrong file's text.
struct NoticeSourceTrees {
    /// Registry checkout root per vendoring crate, keyed by crate name.
    /// Resolved once per run for each distinct crate the roster names.
    registry_crate_roots: BTreeMap<&'static str, PathBuf>,
    /// This repository's root, which `vendor/tatolab-vulkanalia-vma/` hangs off.
    workspace_root: PathBuf,
}

/// One project whose notice has to travel with the binary and which
/// `cargo about` cannot produce, because it is not a package in the resolve
/// graph at all.
struct ProjectWhoseNoticeCargoAboutCannotProduce {
    /// The upstream project's own name, not the directory it lands in.
    display_name: &'static str,
    upstream_repository_url: &'static str,
    /// What the notice actually contains, for the section heading. glslang's is
    /// not a single licence: it is a manifest covering several, which is why
    /// it is 54 KB where most are under 25 KB.
    license_summary: &'static str,
    notice_source: NoticeSource,
    /// Which bullet of the roster this project is named under. Explicit
    /// rather than derived from `notice_source`, because where a notice is
    /// read from and how the code reached the binary are different questions
    /// — two crates share one notice source and sit on different bullets.
    roster: NoticeRosterCargoAboutCannotProduce,
    carried_by: WheelCarryingTheNotice,
}

const PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE:
    &[ProjectWhoseNoticeCargoAboutCannotProduce] = &[
    // The projects below are what `scripts/stage_slang_compiler_libraries.sh`
    // puts in the wheel: `libslang-compiler` and the `libslang-glslang` it
    // opens to optimise SPIR-V, each notice checked in at the revision Slang
    // 2026.12 pins as a submodule.
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Slang",
        upstream_repository_url: "https://github.com/shader-slang/slang",
        license_summary: "Apache-2.0 WITH LLVM-exception",
        // Slang states its copyright only in `REUSE.toml`, so this is that
        // file's copyright lines for the whole tree, then `LICENSE`.
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/Slang/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "DirectX Shader Compiler API header",
        upstream_repository_url: "https://github.com/microsoft/DirectXShaderCompiler",
        license_summary: "NCSA",
        // Slang's `external/dxc/dxcapi.h`, which its compiler core compiles
        // against on every platform: the block heading it, then the licence
        // `REUSE.toml` names for it.
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/DirectXShaderCompiler/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "glslang",
        upstream_repository_url: "https://github.com/KhronosGroup/glslang",
        license_summary: "BSD-3-Clause, BSD-2-Clause, MIT, Apache-2.0, and GPL-3.0 WITH Bison-exception-2.2",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/glslang/LICENSE.txt",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "SPIRV-Tools",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Tools",
        license_summary: "Apache-2.0",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/SPIRV-Tools/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "SPIRV-Headers",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Headers",
        license_summary: "MIT, with an Apache-2.0 carve-out the file names",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/SPIRV-Headers/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "miniz",
        upstream_repository_url: "https://github.com/richgel999/miniz",
        license_summary: "MIT",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/miniz/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "LZ4",
        upstream_repository_url: "https://github.com/lz4/lz4",
        license_summary: "BSD-2-Clause",
        // `lib/LICENSE`: Slang compiles only the library, whose terms differ
        // from the rest of the repository's.
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/LZ4/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "unordered_dense",
        upstream_repository_url: "https://github.com/martinus/unordered_dense",
        license_summary: "MIT",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/unordered_dense/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "fast_float",
        upstream_repository_url: "https://github.com/fastfloat/fast_float",
        license_summary: "MIT",
        // Triple-licensed Apache-2.0, MIT or BSL-1.0; distributed under MIT.
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/fast_float/LICENSE-MIT",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "cmark-gfm",
        upstream_repository_url: "https://github.com/swiftlang/swift-cmark",
        license_summary: "BSD-2-Clause, and the terms of the sources it carries, all reproduced in its `COPYING`",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/slang-compiler-notices/cmark-gfm/COPYING",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheSlangCompiler,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "VulkanMemoryAllocator",
        upstream_repository_url: "https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator",
        license_summary: "MIT",
        notice_source: NoticeSource::LeadingCommentBlockOf {
            header_path_relative_to_workspace_root: "vendor/tatolab-vulkanalia-vma/vendor/VulkanMemoryAllocator/include/vk_mem_alloc.h",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CompiledByTheVulkanaliaVmaForksBuildScript,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Vulkan-Headers",
        upstream_repository_url: "https://github.com/KhronosGroup/Vulkan-Headers",
        // The header states the identifier rather than carrying the terms, so
        // the notice below is the copyright line and the Apache-2.0 text it
        // points at is the one reproduced in full under "Apache License 2.0".
        license_summary: "Apache-2.0, whose full text is reproduced above",
        notice_source: NoticeSource::LeadingCommentBlockOf {
            header_path_relative_to_workspace_root: "vendor/tatolab-vulkanalia-vma/vendor/Vulkan-Headers/include/vulkan/vulkan_core.h",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CompiledByTheVulkanaliaVmaForksBuildScript,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "PipeWire",
        upstream_repository_url: "https://gitlab.freedesktop.org/pipewire/pipewire",
        license_summary: "MIT",
        // Headers only — no PipeWire source is compiled and no PipeWire library
        // is linked. What ships inside the wheel is SPA's `static inline` pod
        // builders and parsers, compiled into
        // `runtime/streamlib-engine/src/linux/pipewire_audio_shim.c`, which is
        // why the terms travel with the binary all the same.
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/pipewire-headers/COPYING",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CompiledByTheEnginesBuildScript,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "libopus",
        upstream_repository_url: "https://gitlab.xiph.org/xiph/opus",
        license_summary: "BSD-3-Clause",
        // libopus's own `COPYING`, inside the sources `opusic-sys` bundles,
        // rather than the crate's own root `LICENSE`, which is opusic-sys's
        // copy of the same text.
        notice_source: NoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name: OPUSIC_VENDORING_CRATE_NAME,
            path_relative_to_crate_root: "opus/COPYING",
        },
        roster: NoticeRosterCargoAboutCannotProduce::LinkedThroughOpusicSys,
        carried_by: WheelCarryingTheNotice::TheExtensionWheelIn(OPUS_EXTENSION_PACKAGE_DIRECTORY),
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "libgit2",
        upstream_repository_url: "https://github.com/libgit2/libgit2",
        license_summary: "GPL-2.0 with the libgit2 linking exception, and the terms of the dependencies it bundles, all reproduced in its `COPYING`",
        notice_source: NoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name: LIBGIT2_VENDORING_CRATE_NAME,
            path_relative_to_crate_root: "libgit2/COPYING",
        },
        roster: NoticeRosterCargoAboutCannotProduce::LinkedThroughLibgit2Sys,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    // The projects below are what `scripts/stage_macos_bundled_vulkan_driver.sh`
    // and `scripts/stage_linux_bundled_vulkan_driver.sh` put in the wheel, each
    // notice checked in at the revision the pinned MoltenVK, Vulkan-Loader,
    // Mesa, LLVM, zstd and manylinux_2_28 zlib build from. A project Mesa or
    // LLVM carries in its own tree under terms of its own has its own entry.
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "MoltenVK",
        upstream_repository_url: "https://github.com/KhronosGroup/MoltenVK",
        license_summary: "Apache-2.0",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/MoltenVK/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheMacOSWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "SPIRV-Cross",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Cross",
        license_summary: "Apache-2.0",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/SPIRV-Cross/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheMacOSWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "cereal",
        upstream_repository_url: "https://github.com/USCiLab/cereal",
        license_summary: "BSD-3-Clause",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/cereal/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheMacOSWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Vulkan-Loader",
        upstream_repository_url: "https://github.com/KhronosGroup/Vulkan-Loader",
        license_summary: "Apache-2.0",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/Vulkan-Loader/LICENSE.txt",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheBundledVulkanLoader,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "cJSON",
        upstream_repository_url: "https://github.com/DaveGamble/cJSON",
        // Compiled into the loader as `loader/cJSON.c`, the one file there
        // under a licence other than Apache-2.0.
        license_summary: "MIT",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/cJSON/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltInTheBundledVulkanLoader,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Mesa",
        upstream_repository_url: "https://gitlab.freedesktop.org/mesa/mesa",
        // Mesa states its copyright file by file, never in one notice, so this
        // is three verbatim parts in order: `docs/license.rst` from its
        // "License / Copyright Information" heading on, every distinct
        // copyright line in the first 60 lines of a C or C++ source under the
        // directories lavapipe's build compiles (tests and the three projects
        // below excepted), and `licenses/MIT`.
        license_summary: "MIT",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/Mesa/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "xxHash",
        upstream_repository_url: "https://github.com/Cyan4973/xxHash",
        // The block heading Mesa's `src/util/xxhash.h`. LLVM's
        // `lib/Support/xxhash.cpp` carries the same notice from the same
        // author, differing only in punctuation and the URLs' scheme.
        license_summary: "BSD-2-Clause",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/xxHash/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Berkeley SoftFloat",
        upstream_repository_url: "https://github.com/ucb-bar/berkeley-softfloat-3",
        // The block heading Mesa's `src/util/softfloat.h`.
        license_summary: "BSD-3-Clause",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/Berkeley-SoftFloat/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Fuchsia radix sort",
        upstream_repository_url: "https://fuchsia.googlesource.com/fuchsia",
        // Mesa's `src/vulkan/runtime/radix_sort/LICENSE`, which lavapipe's
        // acceleration-structure build compiles in.
        license_summary: "BSD-2-Clause",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/Fuchsia-radix-sort/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "LLVM",
        upstream_repository_url: "https://github.com/llvm/llvm-project",
        license_summary: "Apache-2.0 WITH LLVM-exception",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/LLVM/LICENSE.TXT",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "OpenBSD regex",
        upstream_repository_url: "https://github.com/openbsd/src",
        // LLVM's `lib/Support/COPYRIGHT.regex`, covering the Henry Spencer
        // regex sources libLLVMSupport compiles: `regcomp.c`, `regexec.c`,
        // `regerror.c` and `regfree.c`.
        license_summary: "Spencer-94 and BSD-3-Clause",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/OpenBSD-regex/COPYRIGHT.regex",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "OpenBSD strlcpy",
        upstream_repository_url: "https://github.com/openbsd/src",
        // The block heading LLVM's `lib/Support/regstrlcpy.c`.
        license_summary: "ISC",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/OpenBSD-strlcpy/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Unicode ConvertUTF",
        upstream_repository_url: "https://www.unicode.org",
        // The Unicode block in LLVM's `include/llvm/Support/ConvertUTF.h`,
        // which `lib/Support/ConvertUTF.cpp` repeats.
        license_summary: "Unicode-DFS-2015",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/Unicode-ConvertUTF/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Unicode Character Database",
        upstream_repository_url: "https://www.unicode.org/ucd/",
        // The block heading LLVM's
        // `lib/Support/UnicodeNameToCodepointGenerated.cpp`, a table generated
        // from the database's character names.
        license_summary: "Unicode-DFS-2016",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/Unicode-Character-Database/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Zstandard",
        upstream_repository_url: "https://github.com/facebook/zstd",
        // Dual-licensed BSD-3-Clause or GPL-2.0-only; distributed under the former.
        license_summary: "BSD-3-Clause",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/zstd/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "zlib",
        upstream_repository_url: "https://github.com/madler/zlib",
        // 1.2.11 ships no licence file; its terms head `zlib.h`, reproduced
        // here verbatim.
        license_summary: "Zlib",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/bundled-vulkan-driver-notices/zlib/LICENSE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedPrebuiltByTheLinuxWheel,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "CPython",
        upstream_repository_url: "https://github.com/astral-sh/python-build-standalone",
        // `python/licenses/` of python-build-standalone's full archive for the carried build, which names CPython's licence and each library it links; the carried `install_only_stripped` archive keeps only `LICENSE.txt`.
        license_summary: "PSF-2.0, with the licences of the libraries python-build-standalone links",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/runtime-unit-carried-package-notices/CPython/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedInTheRuntimeUnitForNewProjects,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "pip",
        upstream_repository_url: "https://github.com/pypa/pip",
        // Every file under the `pip-*.dist-info/licenses/` the carried CPython archive installs.
        license_summary: "MIT, and the licences of the packages pip vendors",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/runtime-unit-carried-package-notices/pip/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedInTheRuntimeUnitForNewProjects,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "NumPy",
        upstream_repository_url: "https://github.com/numpy/numpy",
        // Every file under each carried wheel's `numpy-*.dist-info/licenses/`; `LICENSE.txt` differs between the platforms only in the paths of the libraries it names.
        license_summary: "BSD-3-Clause, and the licences of the code and libraries its wheels vendor",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/runtime-unit-carried-package-notices/numpy/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedInTheRuntimeUnitForNewProjects,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "numba",
        upstream_repository_url: "https://github.com/numba/numba",
        // Every file under each carried wheel's `numba-*.dist-info/licenses/`.
        license_summary: "BSD-2-Clause, and the licences of the code it vendors",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/runtime-unit-carried-package-notices/numba/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedInTheRuntimeUnitForNewProjects,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "llvmlite",
        upstream_repository_url: "https://github.com/numba/llvmlite",
        // Every file under each carried wheel's `llvmlite-*.dist-info/licenses/`.
        license_summary: "BSD-2-Clause, and LLVM's Apache-2.0 WITH LLVM-exception",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/runtime-unit-carried-package-notices/llvmlite/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedInTheRuntimeUnitForNewProjects,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
    ProjectWhoseNoticeCargoAboutCannotProduce {
        display_name: "Pillow",
        upstream_repository_url: "https://github.com/python-pillow/Pillow",
        // Every file under each carried wheel's `pillow-*.dist-info/licenses/`.
        license_summary: "MIT-CMU, and the licences of the image libraries its wheels vendor",
        notice_source: NoticeSource::VendoredLicenseFile {
            path_relative_to_workspace_root: "vendor/runtime-unit-carried-package-notices/pillow/NOTICE",
        },
        roster: NoticeRosterCargoAboutCannotProduce::CarriedInTheRuntimeUnitForNewProjects,
        carried_by: WheelCarryingTheNotice::TheEngineWheel,
    },
];

/// Which project's notices a run generates.
///
/// The engine workspace and a standalone extension wheel differ in all three
/// things a run has to know — the manifest `cargo about` resolves, where the
/// file lands, and whether the vendored C++ appendix belongs on the end — so
/// they are one choice rather than three flags that could disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticesGenerationTarget {
    /// The engine workspace: the wheel, the SDK crate and everything they link.
    EngineWorkspace,
    /// One standalone extension wheel, by its directory relative to the
    /// workspace root. Not a workspace member, so `cargo about` has to be
    /// pointed at its manifest and reads its own lockfile.
    ExtensionPackage { package_directory: PathBuf },
}

impl NoticesGenerationTarget {
    /// The engine workspace when no extension package was named — which is what
    /// the absent flag means, spelled here so a call site reading `None` does
    /// not have to open this function to learn it.
    pub fn from_optional_extension_package_directory(package_directory: Option<PathBuf>) -> Self {
        match package_directory {
            None => Self::EngineWorkspace,
            Some(package_directory) => Self::ExtensionPackage { package_directory },
        }
    }

    /// Refuse a package directory that is not a plain repository-relative path.
    ///
    /// `Path::join` discards its base when handed an absolute path, so an
    /// absolute or `..`-escaping argument still resolves and still generates —
    /// and the path is written verbatim into the regeneration command the
    /// committed notice carries, which would publish one machine's layout
    /// inside a legal notice.
    fn ensure_the_package_directory_is_repository_relative(&self) -> Result<()> {
        let Self::ExtensionPackage { package_directory } = self else {
            return Ok(());
        };
        anyhow::ensure!(
            package_directory.is_relative()
                && package_directory
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
            "--extension-package-directory takes a repository-relative path such as \
             packages/streamlib-webrtc, not {}: it is written into the regeneration command \
             the generated notices carry",
            package_directory.display()
        );
        Ok(())
    }

    /// The project root: the manifest to resolve, and where the file lands.
    fn project_root(&self, workspace_root: &Path) -> PathBuf {
        match self {
            Self::EngineWorkspace => workspace_root.to_path_buf(),
            Self::ExtensionPackage { package_directory } => workspace_root.join(package_directory),
        }
    }

    /// Whether this target's file appends `project`'s terms.
    fn carries_the_notice_of(&self, project: &ProjectWhoseNoticeCargoAboutCannotProduce) -> bool {
        match (self, project.carried_by) {
            (Self::EngineWorkspace, WheelCarryingTheNotice::TheEngineWheel) => true,
            (
                Self::ExtensionPackage { package_directory },
                WheelCarryingTheNotice::TheExtensionWheelIn(carrying_package_directory),
            ) => package_directory == Path::new(carrying_package_directory),
            _ => false,
        }
    }

    /// The projects on the roster whose terms this target's file appends.
    fn projects_whose_notices_it_carries(
        &self,
    ) -> Vec<&'static ProjectWhoseNoticeCargoAboutCannotProduce> {
        PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE
            .iter()
            .filter(|project| self.carries_the_notice_of(project))
            .collect()
    }

    /// What the appendix opens with, before its roster bullets.
    fn appendix_opening_paragraph(&self) -> &'static str {
        match self {
            Self::EngineWorkspace => {
                "Every project below ships inside the wheel and owes terms the generated half above\n\
                 cannot carry: most are compiled in from vendored sources and so appear in no resolve\n\
                 graph `cargo about` walks, and some ship inside the prebuilt Vulkan loader and\n\
                 driver the macOS or Linux wheel carries, or among the packages the runtime unit\n\
                 carries for new projects. These sections are appended by\n\
                 `cargo xtask generate-third-party-notices`.\n"
            }
            Self::ExtensionPackage { .. } => {
                "Every project below ships inside the wheel and owes terms the generated half above\n\
                 cannot carry: it is compiled in from vendored sources, and so appears in no resolve\n\
                 graph `cargo about` walks. These sections are appended by\n\
                 `cargo xtask generate-third-party-notices`.\n"
            }
        }
    }

    /// Everything above `## Overview`, which the template renders.
    pub fn preamble(&self) -> String {
        let mut preamble = String::from(NOTICES_PREAMBLE_OPENING);
        preamble.push('\n');
        match self {
            Self::EngineWorkspace => {
                preamble.push_str(ENGINE_WORKSPACE_SCOPE_AND_REGENERATION_PROSE)
            }
            Self::ExtensionPackage { package_directory } => {
                preamble.push_str(&EXTENSION_SCOPE_AND_REGENERATION_PROSE_TEMPLATE.replace(
                    EXTENSION_PACKAGE_DIRECTORY_PLACEHOLDER,
                    &package_directory.display().to_string(),
                ))
            }
        }
        preamble.push('\n');
        preamble
    }
}

/// Regenerate one project's notices file in that project's own root.
pub fn run(workspace_root: &Path, target: &NoticesGenerationTarget) -> Result<()> {
    target.ensure_the_package_directory_is_repository_relative()?;

    let project_root = target.project_root(workspace_root);
    let mut notices = target.preamble();
    notices.push_str(&run_cargo_about_generate(
        workspace_root,
        &project_root.join("Cargo.toml"),
        CargoAboutFeatureSelection::EveryFeature,
    )?);

    let mut projects_carried = target.projects_whose_notices_it_carries();
    if matches!(target, NoticesGenerationTarget::ExtensionPackage { .. }) {
        for project in
            projects_an_extension_compiles_in_from_this_repositorys_vendor_tree(&project_root)?
        {
            if !projects_carried
                .iter()
                .any(|carried| std::ptr::eq(*carried, project))
            {
                projects_carried.push(project);
            }
        }
    }
    if !projects_carried.is_empty() {
        let source_trees = NoticeSourceTrees {
            registry_crate_roots: locate_registry_crate_roots(&project_root, &projects_carried)?,
            workspace_root: workspace_root.to_path_buf(),
        };
        notices.push('\n');
        notices.push_str(&render_the_appendix_cargo_about_cannot_produce(
            target,
            &projects_carried,
            &source_trees,
        )?);
    }
    let mut notices_appended = projects_carried.len();
    if matches!(target, NoticesGenerationTarget::ExtensionPackage { .. })
        && let Some((appendix, projects_fetched)) =
            render_the_appendix_of_sources_the_build_script_fetches(&project_root)?
    {
        notices.push('\n');
        notices.push_str(&appendix);
        notices_appended += projects_fetched;
    }
    if matches!(target, NoticesGenerationTarget::ExtensionPackage { .. })
        && root_package_closure_holds(
            &crate::run_cargo_metadata_resolve_document(&project_root)?,
            CEF_BINDINGS_CRATE_NAME,
        )?
    {
        let cef_binary_distribution_root =
            std::env::var_os(CEF_BINARY_DISTRIBUTION_ENVIRONMENT_VARIABLE)
                .map(PathBuf::from)
                .with_context(|| {
                    format!(
                        "{} resolves `{CEF_BINDINGS_CRATE_NAME}`, so its wheel ships CEF: set \
                         {CEF_BINARY_DISTRIBUTION_ENVIRONMENT_VARIABLE} to the extracted CEF \
                         binary distribution whose licence it reproduces",
                        project_root.display()
                    )
                })?;
        notices.push('\n');
        notices.push_str(&render_the_cef_binary_distributions_notice(
            &cef_binary_distribution_holding_libcef(&cef_binary_distribution_root)?,
        )?);
        notices_appended += 1;
    }

    let notices_path = project_root.join(THIRD_PARTY_NOTICES_FILE_NAME);
    std::fs::write(&notices_path, notices)
        .with_context(|| format!("writing {}", notices_path.display()))?;

    tracing::info!(
        "wrote {} ({notices_appended} notices cargo about cannot produce appended)",
        notices_path.display(),
    );
    Ok(())
}

/// Which features `cargo about` resolves a manifest's graph with.
#[derive(Debug, Clone, Copy)]
enum CargoAboutFeatureSelection {
    /// Every feature, the graph `deny.toml` validates.
    EveryFeature,
}

impl CargoAboutFeatureSelection {
    fn cargo_about_arguments(self) -> Vec<&'static str> {
        match self {
            Self::EveryFeature => vec!["--all-features"],
        }
    }
}

/// Render a manifest's Rust closure's notices by shelling out to `cargo about`.
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
fn run_cargo_about_generate(
    workspace_root: &Path,
    manifest_path: &Path,
    feature_selection: CargoAboutFeatureSelection,
) -> Result<String> {
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
        .args(["about", "generate", "--locked"])
        .args(feature_selection.cargo_about_arguments())
        .arg("--config")
        .arg(CARGO_ABOUT_CONFIG_FILE_NAME)
        .arg("--manifest-path")
        .arg(manifest_path)
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

/// Find the registry checkout of every vendoring crate `projects` read a
/// notice out of, through `cargo metadata` on the manifest in `project_root`.
///
/// Resolved rather than hard-coded: the sources live in the registry checkout,
/// whose path carries a version and a registry hash that both move. Driven off
/// the roster itself, so a project added with a new vendoring crate resolves
/// without a second list to keep in step.
fn locate_registry_crate_roots(
    project_root: &Path,
    projects: &[&ProjectWhoseNoticeCargoAboutCannotProduce],
) -> Result<BTreeMap<&'static str, PathBuf>> {
    let metadata = crate::run_cargo_metadata_resolve_document(project_root)?;
    let mut roots = BTreeMap::new();
    for project in projects {
        let NoticeSource::RegistryCrateLicenseFile {
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
                "The notices of the C sources it bundles would be dropped entirely — update the vendoring \
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

/// Render the appended half: one section per project in `projects`.
///
/// A missing or unreadable notice is an error, never an omitted section. The
/// failure mode this guards is a dependency bump or a re-vendor that renames or
/// moves one of the trees — silently shipping the binary without its terms is
/// the one outcome worse than a red build.
fn render_the_appendix_cargo_about_cannot_produce(
    target: &NoticesGenerationTarget,
    projects: &[&ProjectWhoseNoticeCargoAboutCannotProduce],
    source_trees: &NoticeSourceTrees,
) -> Result<String> {
    let mut appendix = String::new();
    // The two rosters come off the table, and no count is stated at all. This
    // paragraph ships inside a legal notice: a seventh project must not be able
    // to leave it enumerating six with every test still green, and a number
    // nobody wrote down is a number that cannot go stale.
    write!(
        appendix,
        "## Notices `cargo about` cannot produce\n\n{}\n{}",
        target.appendix_opening_paragraph(),
        roster_bullets(projects),
    )?;

    for project in projects {
        let notice = read_one_notice(project, source_trees)?;
        write!(
            appendix,
            "\n### {} ({})\n\nUpstream: <{}>\n\n{}",
            project.display_name,
            project.license_summary,
            project.upstream_repository_url,
            fenced_licence_text_block(&notice)
                .with_context(|| format!("fencing the {} notice", project.display_name))?,
        )?;
    }

    Ok(appendix)
}

/// The crate whose build script compiles VulkanMemoryAllocator and the
/// Vulkan-Headers it includes into whatever links it.
const VULKANALIA_VMA_FORK_CRATE_NAME: &str = "tatolab-vulkanalia-vma";

/// The crate whose build script links the CEF binary distribution an
/// extension then ships beside its own library.
const CEF_BINDINGS_CRATE_NAME: &str = "cef-dll-sys";

/// Where the extracted CEF binary distribution is found, as `cef-dll-sys`'s
/// own build reads it.
const CEF_BINARY_DISTRIBUTION_ENVIRONMENT_VARIABLE: &str = "CEF_PATH";

/// The file whose directory is a CEF binary distribution's root.
const CEF_LIBRARY_FILE_NAME: &str = "libcef.so";

/// The licence file at the root of every CEF binary distribution.
const CEF_LICENCE_FILE_NAME: &str = "LICENSE.txt";

/// The file at the root of every CEF binary distribution listing Chromium's
/// own third-party licences, which the extension's wheel ships beside CEF.
const CHROMIUM_CREDITS_FILE_NAME: &str = "CREDITS.html";

/// Whether the closure of the manifest's own package, the crate its wheel
/// builds, holds `package_name`; a workspace member beside it, such as a
/// helper binary staged elsewhere, does not count.
fn root_package_closure_holds(
    cargo_metadata: &serde_json::Value,
    package_name: &str,
) -> Result<bool> {
    let resolve = cargo_metadata
        .get("resolve")
        .context("cargo metadata answered no resolve graph")?;
    let root_package_id = resolve
        .get("root")
        .and_then(serde_json::Value::as_str)
        .context("the manifest has no root package whose closure a wheel builds")?;
    let dependency_ids_by_package_id: std::collections::HashMap<&str, Vec<&str>> = resolve
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .context("cargo metadata's resolve graph has no nodes")?
        .iter()
        .filter_map(|node| {
            let package_id = node.get("id")?.as_str()?;
            let dependency_ids = node
                .get("dependencies")?
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect();
            Some((package_id, dependency_ids))
        })
        .collect();
    let package_names_by_id: std::collections::HashMap<&str, &str> = cargo_metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .context("cargo metadata answered no packages")?
        .iter()
        .filter_map(|package| Some((package.get("id")?.as_str()?, package.get("name")?.as_str()?)))
        .collect();
    let mut package_ids_to_visit = vec![root_package_id];
    let mut package_ids_visited = std::collections::HashSet::new();
    while let Some(package_id) = package_ids_to_visit.pop() {
        if !package_ids_visited.insert(package_id) {
            continue;
        }
        if package_names_by_id.get(package_id) == Some(&package_name) {
            return Ok(true);
        }
        if let Some(dependency_ids) = dependency_ids_by_package_id.get(package_id) {
            package_ids_to_visit.extend(dependency_ids.iter().copied());
        }
    }
    Ok(false)
}

/// Whether the extension's own lockfile resolves `package_name`.
fn extension_lockfile_resolves(extension_project_root: &Path, package_name: &str) -> Result<bool> {
    let lockfile_path = extension_project_root.join("Cargo.lock");
    let lockfile = std::fs::read_to_string(&lockfile_path)
        .with_context(|| format!("reading {}", lockfile_path.display()))?;
    let package_name_line = format!("name = \"{package_name}\"");
    Ok(lockfile
        .lines()
        .any(|line| line.trim() == package_name_line))
}

/// The directory under `cef_path` holding `libcef.so`: `cef_path` itself or
/// the one distribution extracted beneath it, as `stage_cef_runtime.py` finds it.
fn cef_binary_distribution_holding_libcef(cef_path: &Path) -> Result<PathBuf> {
    if cef_path.join(CEF_LIBRARY_FILE_NAME).is_file() {
        return Ok(cef_path.to_path_buf());
    }
    let mut distributions_found: Vec<PathBuf> = walkdir::WalkDir::new(cef_path)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == CEF_LIBRARY_FILE_NAME)
        .filter_map(|entry| entry.path().parent().map(Path::to_path_buf))
        .collect();
    distributions_found.sort();
    match distributions_found.as_slice() {
        [] => anyhow::bail!("no {CEF_LIBRARY_FILE_NAME} under {}", cef_path.display()),
        [only_distribution] => Ok(only_distribution.clone()),
        several => anyhow::bail!(
            "more than one CEF distribution under {}: {several:?}",
            cef_path.display()
        ),
    }
}

/// CEF's own licence, read from the binary distribution the wheel ships, and
/// where Chromium's component licences ride in the wheel beside it.
fn render_the_cef_binary_distributions_notice(
    cef_binary_distribution_root: &Path,
) -> Result<String> {
    let licence_path = cef_binary_distribution_root.join(CEF_LICENCE_FILE_NAME);
    let licence_text = read_notice_file(&licence_path, "Chromium Embedded Framework")?;
    let credits_path = cef_binary_distribution_root.join(CHROMIUM_CREDITS_FILE_NAME);
    anyhow::ensure!(
        credits_path.is_file(),
        "{} is missing, so the wheel would ship Chromium without its credits",
        credits_path.display()
    );
    Ok(format!(
        "## Chromium Embedded Framework and Chromium (BSD-3-Clause)\n\
         \n\
         The wheel ships CEF's binary distribution, which is Chromium. CEF's own\n\
         licence follows. Chromium's and every component it builds in are listed,\n\
         each with its licence, in `{CHROMIUM_CREDITS_FILE_NAME}`, which ships beside\n\
         CEF in the wheel's `tatolab/browser/cef/` directory.\n\
         \n\
         Upstream: <https://github.com/chromiumembedded/cef>\n\
         \n\
         {}",
        fenced_licence_text_block(&licence_text).context("fencing the CEF licence")?,
    ))
}

/// The vendored projects an extension's own lockfile shows it compiles in:
/// the VMA fork's trees when it resolves `tatolab-vulkanalia-vma`.
fn projects_an_extension_compiles_in_from_this_repositorys_vendor_tree(
    extension_project_root: &Path,
) -> Result<Vec<&'static ProjectWhoseNoticeCargoAboutCannotProduce>> {
    let resolves_the_vma_fork =
        extension_lockfile_resolves(extension_project_root, VULKANALIA_VMA_FORK_CRATE_NAME)?;
    Ok(PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE
        .iter()
        .filter(|project| {
            resolves_the_vma_fork
                && project.roster
                    == NoticeRosterCargoAboutCannotProduce::CompiledByTheVulkanaliaVmaForksBuildScript
        })
        .collect())
}

/// The fence every licence text in a notices file is reproduced in.
const LICENCE_TEXT_FENCE: &str = "````";

/// `licence_text` in a [`LICENCE_TEXT_FENCE`] block, refused when the text
/// holds that fence itself and would close it.
pub fn fenced_licence_text_block(licence_text: &str) -> Result<String> {
    anyhow::ensure!(
        !licence_text.contains(LICENCE_TEXT_FENCE),
        "the licence text holds `{LICENCE_TEXT_FENCE}`, which would close the fence it is \
         reproduced in"
    );
    Ok(format!(
        "{LICENCE_TEXT_FENCE}text\n{}\n{LICENCE_TEXT_FENCE}\n",
        licence_text.trim_end()
    ))
}

/// One bullet per roster `projects` fill, each naming the projects that reach
/// the binary that way. Rendered off [`NoticeRosterCargoAboutCannotProduce::ALL`]
/// rather than written out, so a project cannot land in a table the prose above
/// it never mentions.
fn roster_bullets(projects: &[&ProjectWhoseNoticeCargoAboutCannotProduce]) -> String {
    NoticeRosterCargoAboutCannotProduce::ALL
        .iter()
        .map(|roster| joined_project_display_names(projects, *roster))
        .zip(NoticeRosterCargoAboutCannotProduce::ALL)
        .filter(|(names, _)| !names.is_empty())
        .map(|(names, roster)| format!("- {}: {names}\n", roster.how_the_code_reaches_the_binary()))
        .collect()
}

/// The display names of every project in `projects` on one roster, as an
/// English list the surrounding sentence can take.
fn joined_project_display_names(
    projects: &[&ProjectWhoseNoticeCargoAboutCannotProduce],
    roster: NoticeRosterCargoAboutCannotProduce,
) -> String {
    let names: Vec<&str> = projects
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
fn read_one_notice(
    project: &ProjectWhoseNoticeCargoAboutCannotProduce,
    source_trees: &NoticeSourceTrees,
) -> Result<String> {
    let (path, notice) = match project.notice_source {
        NoticeSource::RegistryCrateLicenseFile {
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
        NoticeSource::LeadingCommentBlockOf {
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
        NoticeSource::VendoredLicenseFile {
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
pub(crate) fn leading_comment_block_notice(source: &str) -> Result<String> {
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

    /// What makes a `packages/` directory an extension wheel: a Python project
    /// published under the `tatolab` namespace. Only the tests below read it —
    /// the generator is pointed at one package at a time, and discovery is what
    /// a sweep needs.
    const EXTENSION_DISTRIBUTION_NAME_PREFIX: &str = "tatolab-";

    /// The family idiom for the workspace root in a gate's tests: free, and it
    /// needs neither cargo on PATH nor the package lock.
    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ always has a workspace root above it")
            .to_path_buf()
    }

    /// A stand-in for each vendoring crate's registry checkout, holding every
    /// licence file the roster reads from one, each naming the project it covers.
    ///
    /// Paired with the real workspace root, so the comment-block projects read
    /// the actual vendored headers — `check-vendored-trees` hashes those
    /// trees, so a re-vendor that drops a copyright line fails here.
    fn notice_source_trees_fixture() -> (TempDir, NoticeSourceTrees) {
        let fixture = TempDir::new().expect("temp dir");
        let mut registry_crate_roots = BTreeMap::new();
        for project in PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE {
            let NoticeSource::RegistryCrateLicenseFile {
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
        let source_trees = NoticeSourceTrees {
            registry_crate_roots,
            workspace_root: workspace_root(),
        };
        (fixture, source_trees)
    }

    fn opus_extension_target() -> NoticesGenerationTarget {
        NoticesGenerationTarget::ExtensionPackage {
            package_directory: PathBuf::from(OPUS_EXTENSION_PACKAGE_DIRECTORY),
        }
    }

    fn rendered_appendix_of(
        target: &NoticesGenerationTarget,
        source_trees: &NoticeSourceTrees,
    ) -> Result<String> {
        render_the_appendix_cargo_about_cannot_produce(
            target,
            &target.projects_whose_notices_it_carries(),
            source_trees,
        )
    }

    fn extension_root_whose_lockfile_resolves(package_names: &[&str]) -> TempDir {
        let extension_root = TempDir::new().expect("tempdir");
        let lockfile: String = package_names
            .iter()
            .map(|name| format!("[[package]]\nname = \"{name}\"\nversion = \"0.0.0\"\n\n"))
            .collect();
        fs::write(extension_root.path().join("Cargo.lock"), lockfile).expect("write lockfile");
        extension_root
    }

    #[test]
    fn an_extension_compiling_the_vma_fork_carries_the_trees_it_compiles_in() {
        let extension_root =
            extension_root_whose_lockfile_resolves(&["pyo3", "tatolab-vulkanalia-vma"]);
        let projects = projects_an_extension_compiles_in_from_this_repositorys_vendor_tree(
            extension_root.path(),
        )
        .expect("read lockfile");
        let names: Vec<&str> = projects
            .iter()
            .map(|project| project.display_name)
            .collect();
        assert_eq!(names, ["VulkanMemoryAllocator", "Vulkan-Headers"]);

        let source_trees = NoticeSourceTrees {
            registry_crate_roots: BTreeMap::new(),
            workspace_root: workspace_root(),
        };
        let appendix = render_the_appendix_cargo_about_cannot_produce(
            &opus_extension_target(),
            &projects,
            &source_trees,
        )
        .expect("render");
        assert!(appendix.contains("### VulkanMemoryAllocator (MIT)"));
        assert!(appendix.contains("Copyright"));
    }

    #[test]
    fn an_extension_without_the_vma_fork_carries_no_vendored_appendix() {
        let extension_root = extension_root_whose_lockfile_resolves(&["pyo3", "webrtc"]);
        assert!(
            projects_an_extension_compiles_in_from_this_repositorys_vendor_tree(
                extension_root.path()
            )
            .expect("read lockfile")
            .is_empty()
        );
    }

    fn cargo_metadata_of_a_wheel_crate_beside_a_workspace_member(
        wheel_crate_dependencies: &[&str],
    ) -> serde_json::Value {
        let package = |name: &str| serde_json::json!({"id": format!("{name}-id"), "name": name});
        let node = |name: &str, dependencies: &[&str]| {
            serde_json::json!({
                "id": format!("{name}-id"),
                "dependencies": dependencies.iter().map(|dependency| format!("{dependency}-id")).collect::<Vec<_>>(),
            })
        };
        let mut packages = vec![package("wheel"), package("host"), package("cef-dll-sys")];
        packages.extend(wheel_crate_dependencies.iter().copied().map(package));
        let mut nodes = vec![
            node("wheel", wheel_crate_dependencies),
            node("host", &["cef-dll-sys"]),
            node("cef-dll-sys", &[]),
        ];
        nodes.extend(wheel_crate_dependencies.iter().map(|name| node(name, &[])));
        serde_json::json!({
            "packages": packages,
            "resolve": {"root": "wheel-id", "nodes": nodes},
        })
    }

    #[test]
    fn only_cef_the_wheel_crate_itself_reaches_counts() {
        assert!(
            !root_package_closure_holds(
                &cargo_metadata_of_a_wheel_crate_beside_a_workspace_member(&["pyo3"]),
                CEF_BINDINGS_CRATE_NAME
            )
            .unwrap()
        );
        assert!(
            root_package_closure_holds(
                &cargo_metadata_of_a_wheel_crate_beside_a_workspace_member(&[
                    "pyo3",
                    "cef-dll-sys"
                ]),
                CEF_BINDINGS_CRATE_NAME
            )
            .unwrap()
        );
    }

    #[test]
    fn the_cef_notice_reproduces_the_distributions_licence_and_names_chromiums_credits() {
        let cef_binary_distribution = TempDir::new().expect("tempdir");
        fs::write(
            cef_binary_distribution.path().join(CEF_LICENCE_FILE_NAME),
            "Copyright (c) 2008 Marshall A. Greenblatt.\n",
        )
        .expect("write licence");
        fs::write(
            cef_binary_distribution
                .path()
                .join(CHROMIUM_CREDITS_FILE_NAME),
            "<html/>",
        )
        .expect("write credits");

        let notice = render_the_cef_binary_distributions_notice(cef_binary_distribution.path())
            .expect("render");

        assert!(notice.contains("Marshall A. Greenblatt"));
        assert!(notice.contains(CHROMIUM_CREDITS_FILE_NAME));
    }

    #[test]
    fn the_cef_distribution_is_found_beneath_cef_path_as_the_stager_finds_it() {
        let cef_path = TempDir::new().expect("tempdir");
        let nested_distribution = cef_path.path().join("cef_binary_156_linux64");
        fs::create_dir_all(&nested_distribution).expect("create distribution");
        fs::write(nested_distribution.join(CEF_LIBRARY_FILE_NAME), "").expect("write libcef");

        assert_eq!(
            cef_binary_distribution_holding_libcef(cef_path.path()).expect("find"),
            nested_distribution
        );
        assert_eq!(
            cef_binary_distribution_holding_libcef(&nested_distribution).expect("find"),
            nested_distribution
        );
    }

    #[test]
    fn two_cef_distributions_beneath_cef_path_are_refused() {
        let cef_path = TempDir::new().expect("tempdir");
        for distribution_name in ["first", "second"] {
            let distribution = cef_path.path().join(distribution_name);
            fs::create_dir_all(&distribution).expect("create distribution");
            fs::write(distribution.join(CEF_LIBRARY_FILE_NAME), "").expect("write libcef");
        }

        let refusal = cef_binary_distribution_holding_libcef(cef_path.path()).unwrap_err();

        assert!(
            refusal
                .to_string()
                .contains("more than one CEF distribution")
        );
    }

    #[test]
    fn a_cef_distribution_without_chromiums_credits_is_refused() {
        let cef_binary_distribution = TempDir::new().expect("tempdir");
        fs::write(
            cef_binary_distribution.path().join(CEF_LICENCE_FILE_NAME),
            "Copyright (c) 2008 Marshall A. Greenblatt.\n",
        )
        .expect("write licence");

        let refusal =
            render_the_cef_binary_distributions_notice(cef_binary_distribution.path()).unwrap_err();

        assert!(refusal.to_string().contains(CHROMIUM_CREDITS_FILE_NAME));
    }

    #[test]
    fn the_appendix_reproduces_every_vendored_project_verbatim() {
        let (_fixture, source_trees) = notice_source_trees_fixture();
        let appendix =
            rendered_appendix_of(&NoticesGenerationTarget::EngineWorkspace, &source_trees)
                .expect("render");

        for project in NoticesGenerationTarget::EngineWorkspace.projects_whose_notices_it_carries()
        {
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
        assert!(appendix.contains("Yann Collet"));
        assert!(appendix.contains("The Fuchsia Authors"));
        assert!(appendix.contains("Henry Spencer"));
        assert!(appendix.contains("Todd C. Miller"));
        assert!(appendix.contains("Unicode, Inc."));
    }

    /// `licenses/MIT` alone reads "Copyright (c) <year> <copyright holders>",
    /// which passes the copyright-line check while naming nobody.
    #[test]
    fn the_mesa_notice_names_copyright_holders_beyond_the_mit_template() {
        let (_fixture, source_trees) = notice_source_trees_fixture();
        let mesa = PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE
            .iter()
            .find(|project| project.display_name == "Mesa")
            .expect("Mesa is on the roster");
        let notice = read_one_notice(mesa, &source_trees).expect("read");

        assert!(notice.contains("Copyright © 2019 Red Hat."));
        assert!(notice.contains("Copyright 2008 VMware, Inc."));
        assert!(notice.contains("Permission is hereby granted, free of charge"));
    }

    #[test]
    fn a_moved_licence_file_fails_the_render_naming_the_project() {
        let (fixture, source_trees) = notice_source_trees_fixture();
        let moved = PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE
            .iter()
            .find(|project| project.display_name == "libopus")
            .expect("libopus is one of the vendored projects");
        let NoticeSource::RegistryCrateLicenseFile {
            vendoring_crate_name,
            path_relative_to_crate_root,
        } = moved.notice_source
        else {
            panic!("libopus's notice comes from a registry crate's licence file")
        };
        fs::remove_file(
            fixture
                .path()
                .join(vendoring_crate_name)
                .join(path_relative_to_crate_root),
        )
        .expect("remove");

        let failure = rendered_appendix_of(&opus_extension_target(), &source_trees)
            .expect_err("a missing licence file must not render as an omitted section");
        let reported = format!("{failure:#}");
        assert!(
            reported.contains("libopus"),
            "unhelpful failure: {reported}"
        );
        assert!(
            reported.contains(path_relative_to_crate_root),
            "unhelpful failure: {reported}"
        );
    }

    /// The appendix's opening paragraph ships inside a legal notice, and its
    /// rosters are derived. A notice source added later would render its
    /// projects into sections while leaving them out of every roster — which is
    /// the paragraph claiming a coverage it no longer has.
    #[test]
    fn each_appendix_rosters_name_every_project_it_carries() {
        let (_fixture, source_trees) = notice_source_trees_fixture();
        for target in [
            NoticesGenerationTarget::EngineWorkspace,
            opus_extension_target(),
        ] {
            let projects = target.projects_whose_notices_it_carries();
            assert!(!projects.is_empty(), "{target:?} carries no project");
            let appendix = rendered_appendix_of(&target, &source_trees).expect("render");
            let bullets = roster_bullets(&projects);

            assert!(appendix.contains(&bullets));
            for project in &projects {
                assert!(
                    bullets.contains(project.display_name),
                    "{} has a section in {target:?} but appears in no roster",
                    project.display_name
                );
            }
        }
        for roster in NoticeRosterCargoAboutCannotProduce::ALL {
            let projects: Vec<_> = PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE
                .iter()
                .collect();
            assert!(
                !joined_project_display_names(&projects, roster).is_empty(),
                "{roster:?} names no project, leaving its prose dangling"
            );
        }
    }

    /// Every project's code ships inside exactly one wheel, so exactly one file
    /// appends its terms.
    #[test]
    fn every_project_on_the_roster_is_carried_by_exactly_one_target() {
        let mut targets = vec![NoticesGenerationTarget::EngineWorkspace];
        targets.extend(
            extension_package_directories()
                .into_iter()
                .map(
                    |package_directory| NoticesGenerationTarget::ExtensionPackage {
                        package_directory,
                    },
                ),
        );
        for project in PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE {
            let carrying: Vec<_> = targets
                .iter()
                .filter(|target| target.carries_the_notice_of(project))
                .collect();
            assert_eq!(
                carrying.len(),
                1,
                "{} is carried by {carrying:?}",
                project.display_name
            );
        }
    }

    #[test]
    fn libopus_is_carried_by_the_opus_extension_and_not_the_engine() {
        let libopus = PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE
            .iter()
            .find(|project| project.display_name == "libopus")
            .expect("libopus is on the roster");

        assert!(opus_extension_target().carries_the_notice_of(libopus));
        assert!(!NoticesGenerationTarget::EngineWorkspace.carries_the_notice_of(libopus));
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
        let project = ProjectWhoseNoticeCargoAboutCannotProduce {
            display_name: "Nameless",
            upstream_repository_url: "https://example.invalid",
            license_summary: "none",
            notice_source: NoticeSource::LeadingCommentBlockOf {
                header_path_relative_to_workspace_root: "header.h",
            },
            roster: NoticeRosterCargoAboutCannotProduce::CompiledByTheVulkanaliaVmaForksBuildScript,
            carried_by: WheelCarryingTheNotice::TheEngineWheel,
        };
        let workspace = TempDir::new().expect("temp dir");
        fs::write(workspace.path().join("header.h"), "// just a description\n").expect("write");

        let source_trees = NoticeSourceTrees {
            registry_crate_roots: BTreeMap::new(),
            workspace_root: workspace.path().to_path_buf(),
        };

        let failure = read_one_notice(&project, &source_trees)
            .expect_err("a block with no copyright discharges nothing");
        assert!(
            format!("{failure:#}").contains("no copyright line"),
            "unhelpful failure: {failure:#}"
        );
    }

    #[test]
    fn a_graph_without_the_vendoring_crate_names_what_would_be_dropped() {
        let metadata = serde_json::json!({ "packages": [{ "name": "serde" }] });
        let failure = registry_crate_root_in(&metadata, LIBGIT2_VENDORING_CRATE_NAME)
            .expect_err("no libgit2-sys means its notice vanishes");
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
                { "name": LIBGIT2_VENDORING_CRATE_NAME, "manifest_path": "/a/Cargo.toml" },
                { "name": LIBGIT2_VENDORING_CRATE_NAME, "manifest_path": "/b/Cargo.toml" },
            ]
        });
        let failure = registry_crate_root_in(&metadata, LIBGIT2_VENDORING_CRATE_NAME)
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
                    "name": LIBGIT2_VENDORING_CRATE_NAME,
                    "manifest_path": "/registry/libgit2-sys-0.18.8+1.9.7/Cargo.toml",
                },
                {
                    "name": OPUSIC_VENDORING_CRATE_NAME,
                    "manifest_path": "/registry/opusic-sys-0.7.5/Cargo.toml",
                },
            ]
        });
        assert_eq!(
            registry_crate_root_in(&metadata, LIBGIT2_VENDORING_CRATE_NAME).expect("resolve"),
            PathBuf::from("/registry/libgit2-sys-0.18.8+1.9.7")
        );
        assert_eq!(
            registry_crate_root_in(&metadata, OPUSIC_VENDORING_CRATE_NAME).expect("resolve"),
            PathBuf::from("/registry/opusic-sys-0.7.5")
        );
    }

    /// Each vendoring crate lays its bundled sources out its own way, and the
    /// roster is what says so. A path written for the wrong crate's convention
    /// reads nothing, so the paths are locked rather than left to a reviewer to
    /// notice.
    #[test]
    fn each_vendoring_crates_notice_paths_follow_that_crates_own_layout() {
        for project in PROJECTS_WHOSE_NOTICES_CARGO_ABOUT_CANNOT_PRODUCE {
            let NoticeSource::RegistryCrateLicenseFile {
                vendoring_crate_name,
                path_relative_to_crate_root,
            } = project.notice_source
            else {
                continue;
            };
            match vendoring_crate_name {
                OPUSIC_VENDORING_CRATE_NAME => assert_eq!(
                    path_relative_to_crate_root, "opus/COPYING",
                    "{} reads libopus's own COPYING inside the bundled sources",
                    project.display_name
                ),
                LIBGIT2_VENDORING_CRATE_NAME => assert_eq!(
                    path_relative_to_crate_root, "libgit2/COPYING",
                    "{} reads libgit2's own COPYING inside the bundled sources",
                    project.display_name
                ),
                other => panic!("{other} has no stated notice layout"),
            }
        }
    }

    /// Every path a project's PEP 639 `license-files` names, having first
    /// asserted the notices are among them.
    ///
    /// One helper for both sweeps below, because the two must change together:
    /// `license-files` entries are globs, so the day a project declares
    /// `licenses/*` this join stops being a join — and a fix applied to one
    /// copy would leave the other silently passing.
    fn license_file_paths_naming_the_notices(project_root: &Path) -> Vec<PathBuf> {
        let pyproject_path = project_root.join("pyproject.toml");
        let pyproject: toml::Value =
            toml::from_str(&fs::read_to_string(&pyproject_path).expect("read"))
                .unwrap_or_else(|failure| panic!("{}: {failure}", pyproject_path.display()));

        // Read through `get`, never by index: `toml::Value`'s `Index` panics
        // with "index not found", which is the one failure the message below
        // is written for.
        let declared = pyproject
            .get("project")
            .and_then(|project| project.get("license-files"))
            .and_then(toml::Value::as_array)
            .unwrap_or_else(|| {
                panic!(
                    "{} declares no PEP 639 license-files array",
                    pyproject_path.display()
                )
            });
        assert!(
            declared
                .iter()
                .any(|entry| entry.as_str() == Some(THIRD_PARTY_NOTICES_FILE_NAME)),
            "{} ships third-party code; {THIRD_PARTY_NOTICES_FILE_NAME} must be in license-files",
            project_root.display()
        );

        declared
            .iter()
            .map(|entry| {
                project_root.join(entry.as_str().expect("license-files entries are strings"))
            })
            .collect()
    }

    /// `read_to_string` follows symlinks, so a dangling one fails here rather
    /// than at release time.
    fn assert_every_path_resolves_to_readable_text(paths: &[PathBuf]) {
        for path in paths {
            let contents = fs::read_to_string(path)
                .unwrap_or_else(|failure| panic!("{}: {failure}", path.display()));
            assert!(!contents.trim().is_empty(), "{} is empty", path.display());
        }
    }

    /// The wheel and the SDK crate both reach the notices by symlink, which a
    /// rename or a `git mv` can break without touching a line of this file.
    #[test]
    fn every_artifact_path_that_ships_the_notices_resolves_to_readable_text() {
        let workspace_root = workspace_root();
        let wheel_dir = workspace_root.join("sdk/streamlib-python-wheel");

        let mut paths = license_file_paths_naming_the_notices(&wheel_dir);
        // `cargo package` dereferences a symlink into the archive, so the
        // published `streamlib` crate carries the same bytes beside its source.
        paths.push(
            workspace_root
                .join("sdk/streamlib-sdk")
                .join(THIRD_PARTY_NOTICES_FILE_NAME),
        );

        assert_every_path_resolves_to_readable_text(&paths);
    }

    /// The preamble is the one part of a generated notices file a human
    /// wrote, so a stray edit to it would rewrite the opening of a legal
    /// notice on the next regeneration with nothing to catch it.
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

        assert!(preamble.contains("--extension-package-directory packages/streamlib-webrtc"));
        // Stated against the engine's prose itself rather than two phrases
        // lifted out of it, which would stop failing the moment it is reworded
        // — which is exactly when inheriting it would matter.
        assert!(!preamble.contains(ENGINE_WORKSPACE_SCOPE_AND_REGENERATION_PROSE));
    }

    /// The path is written into the regeneration command a committed legal
    /// notice carries, and `Path::join` would silently take an absolute one.
    #[test]
    fn a_package_directory_outside_the_repository_is_refused_rather_than_baked_in() {
        for rejected in [
            "/home/someone/streamlib/packages/streamlib-webrtc",
            "../elsewhere",
        ] {
            let target = NoticesGenerationTarget::ExtensionPackage {
                package_directory: PathBuf::from(rejected),
            };

            let failure = target
                .ensure_the_package_directory_is_repository_relative()
                .expect_err("an absolute or escaping path is refused");
            assert!(failure.to_string().contains(rejected));
        }

        assert!(
            NoticesGenerationTarget::ExtensionPackage {
                package_directory: PathBuf::from("packages/streamlib-webrtc"),
            }
            .ensure_the_package_directory_is_repository_relative()
            .is_ok()
        );
    }

    /// Every directory under `packages/` whose `pyproject.toml` names a
    /// `tatolab-` distribution, as a repository-relative path.
    ///
    /// Discovered rather than listed, so a second extension is covered the day
    /// its `pyproject.toml` lands. Read off the parsed document rather than by
    /// searching the source text, which would call a package an extension for
    /// naming the prefix in a comment or a URL.
    fn extension_package_directories() -> Vec<PathBuf> {
        let mut directories = Vec::new();
        for entry in fs::read_dir(workspace_root().join("packages")).expect("read packages/") {
            let package_directory = entry.expect("read a packages/ entry").path();
            let Ok(pyproject_source) = fs::read_to_string(package_directory.join("pyproject.toml"))
            else {
                continue;
            };
            let pyproject: toml::Value =
                toml::from_str(&pyproject_source).expect("parse a packages/ pyproject.toml");
            let is_a_tatolab_distribution = pyproject
                .get("project")
                .and_then(|project| project.get("name"))
                .and_then(toml::Value::as_str)
                .is_some_and(|name| name.starts_with(EXTENSION_DISTRIBUTION_NAME_PREFIX));
            if is_a_tatolab_distribution {
                directories.push(
                    PathBuf::from("packages").join(
                        package_directory
                            .file_name()
                            .expect("a packages/ entry has a name"),
                    ),
                );
            }
        }

        assert!(
            !directories.is_empty(),
            "no extension wheel was found under packages/ — the discovery rule moved out from \
             under these tests, which would let an extension ship with no notices unnoticed"
        );
        directories
    }

    /// Every extension wheel discharges its own notice obligation with a file
    /// this generator wrote — not a stale one, and not the engine's.
    #[test]
    fn every_extension_wheel_ships_the_notices_it_declares() {
        let workspace_root = workspace_root();

        for package_directory in extension_package_directories() {
            let project_root = workspace_root.join(&package_directory);
            assert_every_path_resolves_to_readable_text(&license_file_paths_naming_the_notices(
                &project_root,
            ));

            // The same guard the engine's file gets, and for the same reason:
            // the preamble is the one part of a generated legal notice a human
            // wrote, so an edit here would rewrite it on the next regeneration
            // with nothing to catch it.
            let notices_path = project_root.join(THIRD_PARTY_NOTICES_FILE_NAME);
            let committed = fs::read_to_string(&notices_path)
                .unwrap_or_else(|failure| panic!("{}: {failure}", notices_path.display()));
            let expected_preamble = NoticesGenerationTarget::ExtensionPackage {
                package_directory: package_directory.clone(),
            }
            .preamble();
            assert!(
                committed.starts_with(&expected_preamble),
                "{} no longer opens with the preamble this file renders for it",
                notices_path.display()
            );
        }
    }
}
