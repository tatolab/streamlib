// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Regenerates `THIRD-PARTY-NOTICES.md`.
//!
//! Two halves that no single tool covers. `cargo about generate` walks the
//! resolve graph and reproduces each crate's licence text; the four C++
//! projects `shaderc-sys` vendors are appended here, because `cargo about`
//! reads `cargo metadata` and none of shaderc, glslang, SPIRV-Tools or
//! SPIRV-Headers is a package in that graph. They are still distributed: the
//! crate links them into `libshaderc_combined.a`, which is statically linked
//! into the engine and ships inside the wheel.
//!
//! Two of the four do reach the generated half by accident — `cargo about`
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
//!
//! Expect `cargo-about` to warn that the workspace's own crates have no
//! `license` field. That is correct and wanted: they carry `license-file`, and
//! a file titled *third-party* notices is not where our own terms belong.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Generated at the workspace root, and reached from the wheel by symlink so
/// that `.dist-info/licenses/` carries this exact text rather than a copy that
/// can drift from it.
pub const THIRD_PARTY_NOTICES_FILE_NAME: &str = "THIRD-PARTY-NOTICES.md";

/// The handlebars template `cargo about generate` renders. Alongside
/// `about.toml`, which is the config it discovers by convention.
const CARGO_ABOUT_TEMPLATE_FILE_NAME: &str = "about.hbs";

/// The crate whose build directory holds the vendored C++ sources.
const VENDORING_CRATE_NAME: &str = "shaderc-sys";

/// Where that crate extracts them, relative to its own manifest directory.
const VENDORED_CPP_SOURCES_DIR_NAME: &str = "build";

/// One C++ project vendored by [`VENDORING_CRATE_NAME`] and linked into the
/// engine, with the licence text that has to travel with the binary.
struct VendoredCppProjectInShadercSys {
    /// The upstream project's own name, not the directory it lands in.
    display_name: &'static str,
    upstream_repository_url: &'static str,
    /// Relative to `<shaderc-sys>/build/`. Spelled out per project because
    /// glslang's is `LICENSE.txt` while the other three are `LICENSE` — a glob
    /// would hide that, and hiding it is how a rename becomes a dropped notice.
    license_file_relative_path: &'static str,
    /// What the file actually contains, for the section heading. glslang's is
    /// not a single licence: it is a manifest covering several, which is why
    /// it is 54 KB and the others are 11–23 KB.
    license_summary: &'static str,
}

const VENDORED_CPP_PROJECTS: &[VendoredCppProjectInShadercSys] = &[
    VendoredCppProjectInShadercSys {
        display_name: "shaderc",
        upstream_repository_url: "https://github.com/google/shaderc",
        license_file_relative_path: "shaderc/LICENSE",
        license_summary: "Apache-2.0",
    },
    VendoredCppProjectInShadercSys {
        display_name: "glslang",
        upstream_repository_url: "https://github.com/KhronosGroup/glslang",
        license_file_relative_path: "glslang/LICENSE.txt",
        license_summary: "BSD-3-Clause, BSD-2-Clause, MIT, Apache-2.0, and GPL-3.0 WITH Bison-exception-2.2",
    },
    VendoredCppProjectInShadercSys {
        display_name: "SPIRV-Tools",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Tools",
        license_file_relative_path: "spirv-tools/LICENSE",
        license_summary: "Apache-2.0",
    },
    VendoredCppProjectInShadercSys {
        display_name: "SPIRV-Headers",
        upstream_repository_url: "https://github.com/KhronosGroup/SPIRV-Headers",
        license_file_relative_path: "spirv-headers/LICENSE",
        license_summary: "MIT, with an Apache-2.0 carve-out the file names",
    },
];

/// Regenerate the notices file at the workspace root.
pub fn run(workspace_root: &Path) -> Result<()> {
    let generated_rust_closure_notices = run_cargo_about_generate(workspace_root)?;
    let vendored_cpp_sources_dir = locate_vendored_cpp_sources_dir(workspace_root)?;
    let vendored_cpp_appendix = render_vendored_cpp_appendix(&vendored_cpp_sources_dir)?;

    let notices_path = workspace_root.join(THIRD_PARTY_NOTICES_FILE_NAME);
    std::fs::write(
        &notices_path,
        format!("{generated_rust_closure_notices}\n{vendored_cpp_appendix}"),
    )
    .with_context(|| format!("writing {}", notices_path.display()))?;

    tracing::info!(
        "wrote {} ({} vendored C++ projects appended)",
        notices_path.display(),
        VENDORED_CPP_PROJECTS.len()
    );
    Ok(())
}

/// Render the Rust closure's notices by shelling out to `cargo about`.
fn run_cargo_about_generate(workspace_root: &Path) -> Result<String> {
    let output = std::process::Command::new("cargo")
        .args(["about", "generate", CARGO_ABOUT_TEMPLATE_FILE_NAME])
        .current_dir(workspace_root)
        .output()
        .context("spawning `cargo about generate` — `cargo install cargo-about` if missing")?;

    anyhow::ensure!(
        output.status.success(),
        "`cargo about generate` failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim(),
    );

    String::from_utf8(output.stdout).context("`cargo about generate` emitted non-UTF-8")
}

/// Find `<shaderc-sys>/build/` through `cargo metadata`.
///
/// Resolved rather than hard-coded: the sources live in the registry checkout,
/// whose path carries a version and a registry hash that both move.
fn locate_vendored_cpp_sources_dir(workspace_root: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(workspace_root)
        .output()
        .context("running cargo metadata")?;

    anyhow::ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr).trim(),
    );

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing cargo metadata JSON")?;

    let manifest_paths: Vec<&str> = metadata["packages"]
        .as_array()
        .context("cargo metadata has no packages array")?
        .iter()
        .filter(|package| package["name"].as_str() == Some(VENDORING_CRATE_NAME))
        .filter_map(|package| package["manifest_path"].as_str())
        .collect();

    let [manifest_path] = manifest_paths.as_slice() else {
        anyhow::bail!(
            "expected exactly one {VENDORING_CRATE_NAME} in the graph, found {} — \
             the vendored C++ notices would cover only one of them",
            manifest_paths.len()
        );
    };

    let sources_dir = Path::new(manifest_path)
        .parent()
        .context("a manifest path always has a parent directory")?
        .join(VENDORED_CPP_SOURCES_DIR_NAME);

    anyhow::ensure!(
        sources_dir.is_dir(),
        "{} has no {VENDORED_CPP_SOURCES_DIR_NAME}/ directory — run `cargo fetch` first",
        sources_dir.display()
    );

    Ok(sources_dir)
}

/// Render the appended half: one section per vendored C++ project.
///
/// A missing licence file is an error, never an omitted section. The failure
/// mode this guards is a `shaderc-sys` bump that renames or moves one of the
/// four — silently shipping the binary without its terms is the one outcome
/// worse than a red build.
fn render_vendored_cpp_appendix(vendored_cpp_sources_dir: &Path) -> Result<String> {
    let mut appendix = String::from(
        "## Vendored C++ sources\n\
         \n\
         The engine compiles GLSL at runtime through the `shaderc-sys` crate, which vendors the\n\
         four projects below as sources and links them into `libshaderc_combined.a`. That archive\n\
         is statically linked into the engine, so the wheel distributes their compiled form and\n\
         their terms travel with it. None of the four is a package in the Cargo resolve graph, so\n\
         these sections are appended by `cargo xtask generate-third-party-notices` rather than\n\
         found by `cargo about`.\n",
    );

    for project in VENDORED_CPP_PROJECTS {
        let license_file_path = vendored_cpp_sources_dir.join(project.license_file_relative_path);
        let license_text = std::fs::read_to_string(&license_file_path).with_context(|| {
            format!(
                "reading the {} licence at {} — a {VENDORING_CRATE_NAME} upgrade that moved it \
                 would otherwise drop the notice silently",
                project.display_name,
                license_file_path.display()
            )
        })?;

        appendix.push_str(&format!(
            "\n### {} ({})\n\nUpstream: <{}>\n\n````text\n{}\n````\n",
            project.display_name,
            project.license_summary,
            project.upstream_repository_url,
            license_text.trim_end(),
        ));
    }

    Ok(appendix)
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

    /// A stand-in for `<shaderc-sys>/build/` holding every declared licence
    /// file, each with text naming the project it belongs to.
    fn vendored_cpp_sources_fixture() -> TempDir {
        let fixture = TempDir::new().expect("temp dir");
        for project in VENDORED_CPP_PROJECTS {
            let path = fixture.path().join(project.license_file_relative_path);
            fs::create_dir_all(path.parent().expect("a licence file sits in a directory"))
                .expect("fixture dir");
            fs::write(&path, format!("terms covering {}\n", project.display_name))
                .expect("fixture licence file");
        }
        fixture
    }

    #[test]
    fn the_appendix_reproduces_every_vendored_project_verbatim() {
        let fixture = vendored_cpp_sources_fixture();
        let appendix = render_vendored_cpp_appendix(fixture.path()).expect("render");

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
            assert!(
                appendix.contains(&format!("terms covering {}", project.display_name)),
                "{}'s licence text was not reproduced",
                project.display_name
            );
        }
    }

    #[test]
    fn a_moved_licence_file_fails_the_render_naming_the_project() {
        let fixture = vendored_cpp_sources_fixture();
        let moved = VENDORED_CPP_PROJECTS
            .iter()
            .find(|project| project.display_name == "glslang")
            .expect("glslang is one of the four");
        fs::remove_file(fixture.path().join(moved.license_file_relative_path)).expect("remove");

        let failure = render_vendored_cpp_appendix(fixture.path())
            .expect_err("a missing licence file must not render as an omitted section");
        let reported = format!("{failure:#}");
        assert!(
            reported.contains("glslang"),
            "unhelpful failure: {reported}"
        );
        assert!(
            reported.contains(moved.license_file_relative_path),
            "unhelpful failure: {reported}"
        );
    }

    #[test]
    fn glslang_is_the_one_project_whose_licence_file_is_not_named_license() {
        // Not trivia: it is the whole reason the four paths are spelled out
        // instead of globbed, and a future reader deleting the "redundant"
        // extension is exactly the edit this locks.
        let named_license_txt: Vec<&str> = VENDORED_CPP_PROJECTS
            .iter()
            .filter(|project| project.license_file_relative_path.ends_with("LICENSE.txt"))
            .map(|project| project.display_name)
            .collect();
        assert_eq!(named_license_txt, ["glslang"]);
    }

    /// `cargo package` dereferences a symlink and writes the real bytes, so the
    /// published `streamlib` crate carries the notices next to its source — the
    /// same one file, reached the same way as the wheel's. A dangling link here
    /// surfaces at `cargo publish`, which is the worst possible moment.
    #[test]
    fn the_published_sdk_crate_carries_the_notices_beside_its_source() {
        let notices = workspace_root()
            .join("sdk/streamlib-sdk")
            .join(THIRD_PARTY_NOTICES_FILE_NAME);
        let contents = fs::read_to_string(&notices)
            .unwrap_or_else(|failure| panic!("{}: {failure}", notices.display()));
        assert!(
            !contents.trim().is_empty(),
            "{} is empty",
            notices.display()
        );
    }

    /// The notices only discharge anything if they reach the artifact, and the
    /// wheel reaches them by symlink — which a checkout, a rename or a `git mv`
    /// can break without touching a line of this file. `Path::exists` follows
    /// symlinks, so a dangling one fails here rather than at release time.
    #[test]
    fn every_wheel_license_file_resolves_to_readable_text() {
        let wheel_dir = workspace_root().join("sdk/streamlib-python-wheel");
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

        for entry in declared {
            let name = entry.as_str().expect("license-files entries are strings");
            let path = wheel_dir.join(name);
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|failure| panic!("{}: {failure}", path.display()));
            assert!(!contents.trim().is_empty(), "{} is empty", path.display());
        }
    }
}
