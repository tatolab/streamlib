// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Regenerates `THIRD-PARTY-NOTICES.md`.
//!
//! Two halves that no single tool covers. `cargo about generate` walks the
//! resolve graph and reproduces each crate's licence text; the six C++ projects
//! appended here are invisible to it for one shared reason — none of them is a
//! package in that graph — even though every one ends up inside the wheel.
//!
//! Four arrive through `shaderc-sys`, which vendors shaderc, glslang,
//! SPIRV-Tools and SPIRV-Headers as sources and links them into
//! `libshaderc_combined.a`. Two more are checked into this repo:
//! `vendor/tatolab-vulkanalia-vma/build.rs` compiles `wrapper.cpp` against
//! vendored VulkanMemoryAllocator and Vulkan-Headers, and neither of those two
//! trees carries a licence file at all — their copyright line exists only in
//! the comment block heading a header, which is why the notice source is a
//! two-shape enum rather than a path.
//!
//! Two of the six do reach the generated half by accident — `cargo about`
//! scans a crate's own directory for licence files, and `shaderc-sys` extracts
//! its C++ sources under one. They are appended again regardless: there the
//! text is attributed to the crate `shaderc-sys`, and a reader needs to know
//! which upstream project each set of terms actually covers.
//!
//! Not a CI gate, and deliberately so. This needs `cargo-about` installed and
//! reaches the network for crates that ship no licence file of their own. The
//! check that runs on every pull request is `cargo deny check licenses` —
//! a generated file rots, and the gate is what stops a licence outside the
//! allowlist landing quietly in between regenerations.

use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Generated at the workspace root, and reached from the wheel and the SDK
/// crate by symlink so that both ship this exact text rather than a copy that
/// can drift from it.
pub const THIRD_PARTY_NOTICES_FILE_NAME: &str = "THIRD-PARTY-NOTICES.md";

/// The handlebars template `cargo about generate` renders. Alongside
/// `about.toml`, which is the config it discovers by convention.
const CARGO_ABOUT_TEMPLATE_FILE_NAME: &str = "about.hbs";

/// The crate whose build directory holds four of the six vendored C++ trees.
const SHADERC_VENDORING_CRATE_NAME: &str = "shaderc-sys";

/// Where that crate extracts them, relative to its own manifest directory.
const SHADERC_VENDORED_SOURCES_DIR_NAME: &str = "build";

/// Where a vendored C++ project's notice text is read from.
///
/// Two shapes because the two trees genuinely differ, not as a convenience:
/// `shaderc-sys` ships a licence file per project, and the trees the vulkanalia
/// VMA fork vendors ship none.
enum VendoredCppNoticeSource {
    /// A licence file under `<shaderc-sys>/build/`, reproduced whole.
    ShadercSysLicenseFile(&'static str),
    /// The comment block heading a workspace-relative source file — the only
    /// place these projects state their copyright.
    LeadingCommentBlockOf(&'static str),
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
}

const VENDORED_CPP_PROJECTS: &[VendoredCppProjectLinkedIntoTheEngine] = &[
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "shaderc",
        upstream_repository_url: "https://github.com/google/shaderc",
        license_summary: "Apache-2.0",
        notice_source: VendoredCppNoticeSource::ShadercSysLicenseFile("shaderc/LICENSE"),
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "glslang",
        upstream_repository_url: "https://github.com/KhronosGroup/glslang",
        license_summary: "BSD-3-Clause, BSD-2-Clause, MIT, Apache-2.0, and GPL-3.0 WITH Bison-exception-2.2",
        // `LICENSE.txt`, where the other three are `LICENSE`. Spelled out per
        // project rather than globbed for exactly this reason: a glob hides the
        // asymmetry, and hiding it is how a rename becomes a dropped notice.
        notice_source: VendoredCppNoticeSource::ShadercSysLicenseFile("glslang/LICENSE.txt"),
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "SPIRV-Tools",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Tools",
        license_summary: "Apache-2.0",
        notice_source: VendoredCppNoticeSource::ShadercSysLicenseFile("spirv-tools/LICENSE"),
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "SPIRV-Headers",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Headers",
        license_summary: "MIT, with an Apache-2.0 carve-out the file names",
        notice_source: VendoredCppNoticeSource::ShadercSysLicenseFile("spirv-headers/LICENSE"),
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "VulkanMemoryAllocator",
        upstream_repository_url: "https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator",
        license_summary: "MIT",
        notice_source: VendoredCppNoticeSource::LeadingCommentBlockOf(
            "vendor/tatolab-vulkanalia-vma/vendor/VulkanMemoryAllocator/include/vk_mem_alloc.h",
        ),
    },
    VendoredCppProjectLinkedIntoTheEngine {
        display_name: "Vulkan-Headers",
        upstream_repository_url: "https://github.com/KhronosGroup/Vulkan-Headers",
        // The header states the identifier rather than carrying the terms, so
        // the notice below is the copyright line and the Apache-2.0 text it
        // points at is the one reproduced in full under "Apache License 2.0".
        license_summary: "Apache-2.0, whose full text is reproduced above",
        notice_source: VendoredCppNoticeSource::LeadingCommentBlockOf(
            "vendor/tatolab-vulkanalia-vma/vendor/Vulkan-Headers/include/vulkan/vulkan_core.h",
        ),
    },
];

/// Regenerate the notices file at the workspace root.
pub fn run(workspace_root: &Path) -> Result<()> {
    let mut notices = run_cargo_about_generate(workspace_root)?;
    let shaderc_sys_vendored_sources_dir = locate_shaderc_sys_vendored_sources_dir(workspace_root)?;

    notices.push('\n');
    notices.push_str(&render_vendored_cpp_appendix(
        &shaderc_sys_vendored_sources_dir,
        workspace_root,
    )?);

    let notices_path = workspace_root.join(THIRD_PARTY_NOTICES_FILE_NAME);
    std::fs::write(&notices_path, notices)
        .with_context(|| format!("writing {}", notices_path.display()))?;

    tracing::info!(
        "wrote {} ({} vendored C++ projects appended)",
        notices_path.display(),
        VENDORED_CPP_PROJECTS.len()
    );
    Ok(())
}

/// Render the Rust closure's notices by shelling out to `cargo about`.
///
/// Its warnings are surfaced rather than swallowed: "unable to synthesize a
/// license expression for X" is this tool's way of saying a crate reached the
/// artifact with no notice, which is the one thing this file exists to prevent.
/// The workspace's own crates warn on every run and are the expected noise —
/// they carry `license-file`, and a file titled *third-party* notices is not
/// where our own terms belong.
fn run_cargo_about_generate(workspace_root: &Path) -> Result<String> {
    let output = std::process::Command::new("cargo")
        .args(["about", "generate", CARGO_ABOUT_TEMPLATE_FILE_NAME])
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

/// Find `<shaderc-sys>/build/` through `cargo metadata`.
///
/// Resolved rather than hard-coded: the sources live in the registry checkout,
/// whose path carries a version and a registry hash that both move.
fn locate_shaderc_sys_vendored_sources_dir(workspace_root: &Path) -> Result<PathBuf> {
    let metadata = crate::run_cargo_metadata_resolve_document(workspace_root)?;
    let sources_dir = shaderc_sys_vendored_sources_dir_in(&metadata)?;

    anyhow::ensure!(
        sources_dir.is_dir(),
        "{} does not exist — run `cargo fetch` first",
        sources_dir.display()
    );

    Ok(sources_dir)
}

/// The `<shaderc-sys>/build/` path a `cargo metadata` document points at.
///
/// Split from the spawn so both refusals are reachable from a test. They are
/// not hypothetical: renaming, replacing or feature-gating the vendoring crate
/// silently drops four notices, and two copies of it in one graph means the
/// appendix covers whichever the iterator saw first.
fn shaderc_sys_vendored_sources_dir_in(metadata: &serde_json::Value) -> Result<PathBuf> {
    let manifest_paths: Vec<&str> = metadata["packages"]
        .as_array()
        .context("cargo metadata has no packages array")?
        .iter()
        .filter(|package| package["name"].as_str() == Some(SHADERC_VENDORING_CRATE_NAME))
        .filter_map(|package| package["manifest_path"].as_str())
        .collect();

    let [manifest_path] = manifest_paths.as_slice() else {
        anyhow::bail!(
            "expected exactly one {SHADERC_VENDORING_CRATE_NAME} in the graph, found {}. {}",
            manifest_paths.len(),
            if manifest_paths.is_empty() {
                "Four vendored C++ notices would be dropped entirely — update \
                 SHADERC_VENDORING_CRATE_NAME if the GLSL compiler moved to another crate"
            } else {
                "The appendix would cover whichever copy came first, and say nothing about \
                 the rest"
            }
        );
    };

    Ok(Path::new(manifest_path)
        .parent()
        .with_context(|| format!("{manifest_path} has no parent directory"))?
        .join(SHADERC_VENDORED_SOURCES_DIR_NAME))
}

/// Render the appended half: one section per vendored C++ project.
///
/// A missing or unreadable notice is an error, never an omitted section. The
/// failure mode this guards is a dependency bump or a re-vendor that renames or
/// moves one of the six — silently shipping the binary without its terms is the
/// one outcome worse than a red build.
fn render_vendored_cpp_appendix(
    shaderc_sys_vendored_sources_dir: &Path,
    workspace_root: &Path,
) -> Result<String> {
    let mut appendix = String::from(
        "## Vendored C++ sources\n\
         \n\
         The six projects below are compiled into the engine from vendored sources rather than\n\
         linked as Cargo packages, so none of them appears in the resolve graph `cargo about`\n\
         walks — and every one of them ships inside the wheel. shaderc, glslang, SPIRV-Tools and\n\
         SPIRV-Headers arrive through the `shaderc-sys` crate, which links them into\n\
         `libshaderc_combined.a`; VulkanMemoryAllocator and Vulkan-Headers are checked into this\n\
         repository and compiled by `vendor/tatolab-vulkanalia-vma/build.rs`. These sections are\n\
         appended by `cargo xtask generate-third-party-notices`.\n",
    );

    for project in VENDORED_CPP_PROJECTS {
        let notice =
            read_vendored_cpp_notice(project, shaderc_sys_vendored_sources_dir, workspace_root)?;
        write!(
            appendix,
            "\n### {} ({})\n\nUpstream: <{}>\n\n````text\n{}\n````\n",
            project.display_name,
            project.license_summary,
            project.upstream_repository_url,
            notice.trim_end(),
        )
        .expect("writing to a String never fails");
    }

    Ok(appendix)
}

/// Read one project's notice text from whichever tree holds it.
fn read_vendored_cpp_notice(
    project: &VendoredCppProjectLinkedIntoTheEngine,
    shaderc_sys_vendored_sources_dir: &Path,
    workspace_root: &Path,
) -> Result<String> {
    let (path, notice) = match project.notice_source {
        VendoredCppNoticeSource::ShadercSysLicenseFile(relative_path) => {
            let path = shaderc_sys_vendored_sources_dir.join(relative_path);
            let text = read_notice_file(&path, project.display_name)?;
            (path, text)
        }
        VendoredCppNoticeSource::LeadingCommentBlockOf(relative_path) => {
            let path = workspace_root.join(relative_path);
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
/// Vulkan-Headers puts it after the include guard.
fn leading_comment_block_notice(source: &str) -> Result<String> {
    let mut lines = source
        .lines()
        .skip_while(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
        .peekable();

    let first = *lines
        .peek()
        .context("the file holds no comment block at all")?;

    let block: Vec<&str> = if first.trim_start().starts_with("//") {
        lines
            .take_while(|line| line.trim_start().starts_with("//"))
            .collect()
    } else if first.trim_start().starts_with("/*") {
        let mut collected = Vec::new();
        for line in lines {
            let terminates = line.contains("*/");
            collected.push(line);
            if terminates {
                break;
            }
        }
        anyhow::ensure!(
            collected.last().is_some_and(|line| line.contains("*/")),
            "the leading block comment is never closed"
        );
        collected
    } else {
        anyhow::bail!("the first non-directive line is not a comment: {first:?}");
    };

    Ok(block
        .iter()
        .map(|line| {
            line.trim()
                .trim_start_matches("//")
                .trim_start_matches("/*")
                .trim_end_matches("*/")
                .trim_start_matches('*')
                .trim()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned())
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
    /// four shaderc-side projects declare, each naming the project it covers.
    fn shaderc_sys_sources_fixture() -> TempDir {
        let fixture = TempDir::new().expect("temp dir");
        for project in VENDORED_CPP_PROJECTS {
            let VendoredCppNoticeSource::ShadercSysLicenseFile(relative_path) =
                project.notice_source
            else {
                continue;
            };
            let path = fixture.path().join(relative_path);
            fs::create_dir_all(path.parent().expect("a licence file sits in a directory"))
                .expect("fixture dir");
            fs::write(
                &path,
                format!("Copyright (c) terms covering {}\n", project.display_name),
            )
            .expect("fixture licence file");
        }
        fixture
    }

    #[test]
    fn the_appendix_reproduces_every_vendored_project_verbatim() {
        let fixture = shaderc_sys_sources_fixture();
        // The two comment-block projects read the real vendored headers, which
        // `check-vendored-vulkanalia` pins byte-for-byte — so this also fails
        // if a re-vendor drops their copyright line.
        let appendix =
            render_vendored_cpp_appendix(fixture.path(), &workspace_root()).expect("render");

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
    }

    #[test]
    fn a_moved_licence_file_fails_the_render_naming_the_project() {
        let fixture = shaderc_sys_sources_fixture();
        let moved = VENDORED_CPP_PROJECTS
            .iter()
            .find(|project| project.display_name == "glslang")
            .expect("glslang is one of the six");
        let VendoredCppNoticeSource::ShadercSysLicenseFile(relative_path) = moved.notice_source
        else {
            panic!("glslang's notice comes from a shaderc-sys licence file")
        };
        fs::remove_file(fixture.path().join(relative_path)).expect("remove");

        let failure = render_vendored_cpp_appendix(fixture.path(), &workspace_root())
            .expect_err("a missing licence file must not render as an omitted section");
        let reported = format!("{failure:#}");
        assert!(
            reported.contains("glslang"),
            "unhelpful failure: {reported}"
        );
        assert!(
            reported.contains(relative_path),
            "unhelpful failure: {reported}"
        );
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
                    VendoredCppNoticeSource::ShadercSysLicenseFile(path)
                        if path.ends_with("LICENSE.txt")
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
        let fixture = shaderc_sys_sources_fixture();
        let project = VendoredCppProjectLinkedIntoTheEngine {
            display_name: "Nameless",
            upstream_repository_url: "https://example.invalid",
            license_summary: "none",
            notice_source: VendoredCppNoticeSource::LeadingCommentBlockOf("header.h"),
        };
        let workspace = TempDir::new().expect("temp dir");
        fs::write(workspace.path().join("header.h"), "// just a description\n").expect("write");

        let failure = read_vendored_cpp_notice(&project, fixture.path(), workspace.path())
            .expect_err("a block with no copyright discharges nothing");
        assert!(
            format!("{failure:#}").contains("no copyright line"),
            "unhelpful failure: {failure:#}"
        );
    }

    #[test]
    fn a_graph_without_the_vendoring_crate_names_what_would_be_dropped() {
        let metadata = serde_json::json!({ "packages": [{ "name": "serde" }] });
        let failure = shaderc_sys_vendored_sources_dir_in(&metadata)
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
        let failure = shaderc_sys_vendored_sources_dir_in(&metadata)
            .expect_err("two copies means the appendix silently covers one");
        assert!(
            format!("{failure:#}").contains("found 2"),
            "unhelpful failure: {failure:#}"
        );
    }

    #[test]
    fn the_vendoring_crates_build_directory_is_what_gets_scanned() {
        let metadata = serde_json::json!({
            "packages": [{
                "name": SHADERC_VENDORING_CRATE_NAME,
                "manifest_path": "/registry/shaderc-sys-0.10.1/Cargo.toml",
            }]
        });
        assert_eq!(
            shaderc_sys_vendored_sources_dir_in(&metadata).expect("resolve"),
            PathBuf::from("/registry/shaderc-sys-0.10.1/build")
        );
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
}
