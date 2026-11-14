// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The notices of C and C++ projects an extension wheel's own build script
//! fetches and compiles in. They are no package in the resolve graph and no
//! tree in this repository, so their licence files are read out of the
//! sources the package's last build fetched.

use crate::generate_third_party_notices::fenced_licence_text_block;
use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Where one fetched project's tree sits inside the build script's `OUT_DIR`.
enum FetchedSourceTreeLocation {
    /// A directory the build script names exactly.
    DirectoryInTheBuildOutput {
        path_relative_to_build_output: &'static str,
    },
    /// The one directory under `parent_relative_to_build_output` whose name
    /// starts with `directory_name_prefix`; the rest of the name is the tag a
    /// fetching build file pins, which a bump changes.
    OnlyDirectoryNamedWithPrefix {
        parent_relative_to_build_output: &'static str,
        directory_name_prefix: &'static str,
    },
}

/// One project an extension's build script fetches and compiles into its wheel.
struct ProjectFetchedByAnExtensionsBuildScript {
    display_name: &'static str,
    upstream_repository_url: &'static str,
    license_summary: &'static str,
    source_tree: FetchedSourceTreeLocation,
    license_file_relative_to_the_source_tree: &'static str,
}

/// An extension wheel whose build script fetches the sources it compiles in.
struct ExtensionWhoseBuildScriptFetchesSources {
    /// The package directory's name under `packages/`, which is also the
    /// crate whose `OUT_DIR` holds the fetched trees.
    package_directory_name: &'static str,
    projects: &'static [ProjectFetchedByAnExtensionsBuildScript],
}

/// Where rive-runtime's premake build clones the dependencies it compiles.
const RIVE_RUNTIME_DEPENDENCIES_DIRECTORY: &str = "rive-bridge-workspace/dependencies";

const fn rive_runtime_dependency(
    display_name: &'static str,
    upstream_repository_url: &'static str,
    license_summary: &'static str,
    directory_name_prefix: &'static str,
    license_file_relative_to_the_source_tree: &'static str,
) -> ProjectFetchedByAnExtensionsBuildScript {
    ProjectFetchedByAnExtensionsBuildScript {
        display_name,
        upstream_repository_url,
        license_summary,
        source_tree: FetchedSourceTreeLocation::OnlyDirectoryNamedWithPrefix {
            parent_relative_to_build_output: RIVE_RUNTIME_DEPENDENCIES_DIRECTORY,
            directory_name_prefix,
        },
        license_file_relative_to_the_source_tree,
    }
}

const EXTENSIONS_WHOSE_BUILD_SCRIPTS_FETCH_SOURCES: &[ExtensionWhoseBuildScriptFetchesSources] =
    &[ExtensionWhoseBuildScriptFetchesSources {
        package_directory_name: "tatolab-rive",
        projects: &[
            ProjectFetchedByAnExtensionsBuildScript {
                display_name: "rive-runtime",
                upstream_repository_url: "https://github.com/rive-app/rive-runtime",
                license_summary: "MIT",
                source_tree: FetchedSourceTreeLocation::DirectoryInTheBuildOutput {
                    path_relative_to_build_output: "rive-runtime",
                },
                license_file_relative_to_the_source_tree: "LICENSE",
            },
            rive_runtime_dependency(
                "HarfBuzz",
                "https://github.com/rive-app/harfbuzz",
                "MIT-style \"Old MIT\"",
                "rive-app_harfbuzz_",
                "COPYING",
            ),
            rive_runtime_dependency(
                "SheenBidi",
                "https://github.com/Tehreer/SheenBidi",
                "Apache-2.0",
                "Tehreer_SheenBidi_",
                "LICENSE",
            ),
            rive_runtime_dependency(
                "Yoga",
                "https://github.com/rive-app/yoga",
                "MIT",
                "rive-app_yoga_",
                "LICENSE",
            ),
            rive_runtime_dependency(
                "libpng",
                "https://github.com/glennrp/libpng",
                "libpng-2.0",
                "glennrp_libpng_",
                "LICENSE",
            ),
            rive_runtime_dependency(
                "zlib",
                "https://github.com/madler/zlib",
                "Zlib",
                "madler_zlib_",
                "LICENSE",
            ),
            rive_runtime_dependency(
                "libjpeg",
                "https://github.com/rive-app/libjpeg",
                "IJG",
                "rive-app_libjpeg_",
                "README",
            ),
            rive_runtime_dependency(
                "libwebp",
                "https://github.com/webmproject/libwebp",
                "BSD-3-Clause",
                "webmproject_libwebp_",
                "COPYING",
            ),
            rive_runtime_dependency(
                "VulkanMemoryAllocator",
                "https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator",
                "MIT",
                "GPUOpen-LibrariesAndSDKs_VulkanMemoryAllocator_",
                "LICENSE.txt",
            ),
            rive_runtime_dependency(
                "miniaudio",
                "https://github.com/rive-app/miniaudio",
                "Unlicense or MIT-0",
                "rive-app_miniaudio_",
                "LICENSE",
            ),
        ],
    }];

/// The appendix of projects the extension at `extension_project_root` fetches
/// in its build script, or `None` when its build script fetches none.
pub fn render_the_appendix_of_sources_the_build_script_fetches(
    extension_project_root: &Path,
) -> Result<Option<(String, usize)>> {
    let Some(extension) = extension_whose_build_script_fetches_sources(extension_project_root)
    else {
        return Ok(None);
    };
    let build_output = newest_build_output_of(extension_project_root, extension)?;
    let mut appendix = String::from(
        "## Sources the build script fetches\n\
         \n\
         Every project below is fetched at a pinned revision and compiled into the wheel by\n\
         its own `build.rs`, so it appears in no resolve graph `cargo about` walks. These\n\
         sections are appended by `cargo xtask generate-third-party-notices` from the\n\
         sources the package's last build fetched.\n",
    );
    for project in extension.projects {
        let notice = read_the_fetched_notice(project, &build_output)?;
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
    Ok(Some((appendix, extension.projects.len())))
}

fn extension_whose_build_script_fetches_sources(
    extension_project_root: &Path,
) -> Option<&'static ExtensionWhoseBuildScriptFetchesSources> {
    let package_directory_name = extension_project_root.file_name()?.to_str()?;
    EXTENSIONS_WHOSE_BUILD_SCRIPTS_FETCH_SOURCES
        .iter()
        .find(|extension| extension.package_directory_name == package_directory_name)
}

/// The extension's Cargo target directory: `CARGO_TARGET_DIR` when set,
/// otherwise its own `target/`.
fn cargo_target_directory_of(extension_project_root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(configured) => extension_project_root.join(configured),
        None => extension_project_root.join("target"),
    }
}

/// Every `build/` directory under a Cargo target directory: `<profile>/build`
/// for a host build and `<triple>/<profile>/build` for a `--target` build.
fn build_script_directories_under(target_directory: &Path) -> Vec<PathBuf> {
    read_directory_entries(target_directory)
        .into_iter()
        .flat_map(|profile_or_triple| {
            std::iter::once(profile_or_triple.join("build")).chain(
                read_directory_entries(&profile_or_triple)
                    .into_iter()
                    .map(|profile| profile.join("build")),
            )
        })
        .filter(|build_directory| build_directory.is_dir())
        .collect()
}

/// The most recently written `OUT_DIR` of the extension's build script under
/// its Cargo target directory, across every profile and target triple.
fn newest_build_output_of(
    extension_project_root: &Path,
    extension: &ExtensionWhoseBuildScriptFetchesSources,
) -> Result<PathBuf> {
    let build_script_directory_prefix = format!("{}-", extension.package_directory_name);
    let target_directory = cargo_target_directory_of(extension_project_root);
    let mut build_outputs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for build_directory in build_script_directories_under(&target_directory) {
        for build_script_directory in read_directory_entries(&build_directory) {
            let is_this_extensions_build_script = build_script_directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&build_script_directory_prefix));
            let build_output = build_script_directory.join("out");
            if !is_this_extensions_build_script || !build_output.is_dir() {
                continue;
            }
            let modified = build_output
                .metadata()
                .and_then(|metadata| metadata.modified())
                .with_context(|| format!("reading when {} changed", build_output.display()))?;
            build_outputs.push((modified, build_output));
        }
    }
    build_outputs
        .into_iter()
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, build_output)| build_output)
        .with_context(|| {
            format!(
                "no build of {} under {} holds the sources its build script fetches — build \
                 the package first (`cargo build --lib` in it), then regenerate",
                extension.package_directory_name,
                target_directory.display()
            )
        })
}

fn read_directory_entries(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn read_the_fetched_notice(
    project: &ProjectFetchedByAnExtensionsBuildScript,
    build_output: &Path,
) -> Result<String> {
    let source_tree = match project.source_tree {
        FetchedSourceTreeLocation::DirectoryInTheBuildOutput {
            path_relative_to_build_output,
        } => build_output.join(path_relative_to_build_output),
        FetchedSourceTreeLocation::OnlyDirectoryNamedWithPrefix {
            parent_relative_to_build_output,
            directory_name_prefix,
        } => only_directory_named_with_prefix(
            &build_output.join(parent_relative_to_build_output),
            directory_name_prefix,
        )
        .with_context(|| format!("locating the {} sources", project.display_name))?,
    };
    let path = source_tree.join(project.license_file_relative_to_the_source_tree);
    let notice = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading the {} notice at {} — a bump that moved it would otherwise drop the \
             notice silently",
            project.display_name,
            path.display()
        )
    })?;
    anyhow::ensure!(
        notice.contains("Copyright") || notice.contains("copyright"),
        "the {} notice at {} carries no copyright line — reproducing it would discharge nothing",
        project.display_name,
        path.display()
    );
    Ok(notice)
}

fn only_directory_named_with_prefix(parent: &Path, directory_name_prefix: &str) -> Result<PathBuf> {
    let matching: Vec<PathBuf> = read_directory_entries(parent)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(directory_name_prefix))
        })
        .collect();
    match matching.as_slice() {
        [only] => Ok(only.clone()),
        [] => anyhow::bail!(
            "no directory under {} starts with {directory_name_prefix}",
            parent.display()
        ),
        several => anyhow::bail!(
            "{} directories under {} start with {directory_name_prefix}, so which one the \
             wheel compiled is ambiguous: {}",
            several.len(),
            parent.display(),
            several
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn rive_extension() -> &'static ExtensionWhoseBuildScriptFetchesSources {
        EXTENSIONS_WHOSE_BUILD_SCRIPTS_FETCH_SOURCES
            .iter()
            .find(|extension| extension.package_directory_name == "tatolab-rive")
            .expect("tatolab-rive is on the roster")
    }

    /// A `packages/tatolab-rive` whose last build fetched every rostered tree,
    /// each licence file naming the project it covers.
    fn rive_package_with_a_build_that_fetched_every_project() -> TempDir {
        let packages = TempDir::new().expect("temp dir");
        let build_output = packages
            .path()
            .join("tatolab-rive/target/debug/build/tatolab-rive-0123456789abcdef/out");
        for project in rive_extension().projects {
            let source_tree = match project.source_tree {
                FetchedSourceTreeLocation::DirectoryInTheBuildOutput {
                    path_relative_to_build_output,
                } => build_output.join(path_relative_to_build_output),
                FetchedSourceTreeLocation::OnlyDirectoryNamedWithPrefix {
                    parent_relative_to_build_output,
                    directory_name_prefix,
                } => build_output
                    .join(parent_relative_to_build_output)
                    .join(format!("{directory_name_prefix}v1.0")),
            };
            fs::create_dir_all(&source_tree).expect("fixture tree");
            fs::write(
                source_tree.join(project.license_file_relative_to_the_source_tree),
                format!("Copyright (c) terms covering {}\n", project.display_name),
            )
            .expect("fixture licence file");
        }
        packages
    }

    #[test]
    fn the_rive_appendix_reproduces_every_fetched_project() {
        let packages = rive_package_with_a_build_that_fetched_every_project();
        let (appendix, project_count) = render_the_appendix_of_sources_the_build_script_fetches(
            &packages.path().join("tatolab-rive"),
        )
        .expect("render")
        .expect("tatolab-rive fetches sources");
        assert_eq!(project_count, rive_extension().projects.len());
        for project in rive_extension().projects {
            assert!(
                appendix.contains(&format!("terms covering {}", project.display_name)),
                "{} is missing from the appendix",
                project.display_name
            );
        }
        assert!(appendix.contains("### rive-runtime (MIT)"));
    }

    #[test]
    fn an_extension_whose_build_script_fetches_nothing_carries_no_fetched_appendix() {
        let packages = TempDir::new().expect("temp dir");
        assert!(
            render_the_appendix_of_sources_the_build_script_fetches(
                &packages.path().join("tatolab-camera")
            )
            .expect("render")
            .is_none()
        );
    }

    #[test]
    fn a_target_triple_build_is_found_beside_a_host_build() {
        let target_directory = TempDir::new().unwrap();
        let host_build = target_directory.path().join("release/build");
        let triple_build = target_directory
            .path()
            .join("x86_64-unknown-linux-gnu/release/build");
        fs::create_dir_all(&host_build).unwrap();
        fs::create_dir_all(&triple_build).unwrap();

        let mut found = build_script_directories_under(target_directory.path());
        found.sort();
        let mut expected = vec![host_build, triple_build];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn an_unbuilt_extension_is_refused_naming_the_build_it_needs() {
        let packages = TempDir::new().expect("temp dir");
        let refusal = render_the_appendix_of_sources_the_build_script_fetches(
            &packages.path().join("tatolab-rive"),
        )
        .expect_err("nothing was fetched");
        assert!(format!("{refusal:#}").contains("build the package first"));
    }

    #[test]
    fn two_fetched_tags_of_one_project_are_refused_as_ambiguous() {
        let packages = rive_package_with_a_build_that_fetched_every_project();
        let dependencies = packages
            .path()
            .join("tatolab-rive/target/debug/build/tatolab-rive-0123456789abcdef/out");
        fs::create_dir_all(
            dependencies
                .join(RIVE_RUNTIME_DEPENDENCIES_DIRECTORY)
                .join("madler_zlib_v0.9"),
        )
        .expect("second zlib");
        let refusal = render_the_appendix_of_sources_the_build_script_fetches(
            &packages.path().join("tatolab-rive"),
        )
        .expect_err("two zlib trees");
        assert!(format!("{refusal:#}").contains("ambiguous"));
    }
}
