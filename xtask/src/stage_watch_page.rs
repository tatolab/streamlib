// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask stage-watch-page`: the preview page, built from
//! `runtime/tatolab-watch/` with the bun its `package.json` pins and the
//! dependencies its `bun.lock` pins, into the directory the runtime's loopback
//! HTTP serves under `/watch`.
//!
//! The staged directory holds the page's entry file, the one bundle `bun build`
//! makes of its script, and the Opus decoder polyfill's WebAssembly build,
//! which libav.js fetches by URL from beside the bundle and no bundler inlines.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The preview page's web project, relative to the workspace.
pub const WATCH_PAGE_PROJECT_RELATIVE_TO_WORKSPACE: &str = "runtime/tatolab-watch";

/// The page's entry file, in the project and in the staged directory alike.
const WATCH_PAGE_ENTRY_FILE_NAME: &str = "index.html";

/// The page's one script, which `bun build` bundles.
const WATCH_PAGE_SCRIPT_FILE_NAME: &str = "main.ts";

/// The bundle `bun build` writes of [`WATCH_PAGE_SCRIPT_FILE_NAME`].
const WATCH_PAGE_BUNDLE_FILE_NAME: &str = "main.js";

/// Where the entry file loads the bundle from: absolute under the loopback
/// HTTP's `/watch` route, since the entry file answers at every address below
/// it.
const WATCH_PAGE_BUNDLE_PATH_THE_ENTRY_FILE_LOADS: &str = "/watch/main.js";

/// The `package.json` field naming the bun the page is built with, as
/// `bun@<version>`; `setup-bun` reads the same field.
const PACKAGE_MANAGER_FIELD: &str = "packageManager";

/// The polyfill package's built files, relative to the project.
const OPUS_DECODER_POLYFILL_DIST_RELATIVE_TO_THE_PROJECT: &str =
    "node_modules/@libav.js/variant-opus-af/dist";

/// The suffixes of the polyfill's WebAssembly loader and module, the build
/// libav.js loads when the page is not cross-origin isolated.
const OPUS_DECODER_POLYFILL_FILE_SUFFIXES_LOADED_BESIDE_THE_BUNDLE: [&str; 2] =
    [".wasm.mjs", ".wasm.wasm"];

/// The preview page's web project in `workspace_root`.
pub fn watch_page_project_in_the_workspace(workspace_root: &Path) -> PathBuf {
    workspace_root.join(WATCH_PAGE_PROJECT_RELATIVE_TO_WORKSPACE)
}

/// The bun version `package_json` pins in its `packageManager` field.
pub fn pinned_bun_version_from_package_json(package_json: &str) -> Result<String> {
    let package: serde_json::Value =
        serde_json::from_str(package_json).context("parsing the preview page's package.json")?;
    let package_manager = package
        .get(PACKAGE_MANAGER_FIELD)
        .and_then(serde_json::Value::as_str)
        .with_context(|| {
            format!("the preview page's package.json names no `{PACKAGE_MANAGER_FIELD}`")
        })?;
    match package_manager.strip_prefix("bun@") {
        Some(version) if !version.is_empty() => Ok(version.to_string()),
        _ => bail!(
            "the preview page's package.json `{PACKAGE_MANAGER_FIELD}` is `{package_manager}`, \
             not `bun@<version>`"
        ),
    }
}

/// Refuse a `bun` on `PATH` other than the one the project pins, so every
/// build of the page is made by the same bundler.
fn ensure_the_pinned_bun_is_on_the_path(project_directory: &Path) -> Result<()> {
    let package_json_path = project_directory.join("package.json");
    let package_json = std::fs::read_to_string(&package_json_path)
        .with_context(|| format!("reading {}", package_json_path.display()))?;
    let pinned_bun_version = pinned_bun_version_from_package_json(&package_json)?;
    let how_to_install_it = format!(
        "install it with `curl -fsSL https://bun.sh/install | bash -s bun-v{pinned_bun_version}`"
    );
    let bun_version_output = std::process::Command::new("bun")
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "running `bun --version`: the preview page is built with bun \
                 {pinned_bun_version}; {how_to_install_it}"
            )
        })?;
    let bun_version_on_the_path = String::from_utf8_lossy(&bun_version_output.stdout)
        .trim()
        .to_string();
    if bun_version_on_the_path != pinned_bun_version {
        bail!(
            "the preview page is built with bun {pinned_bun_version}, and the bun on PATH is \
             `{bun_version_on_the_path}`; {how_to_install_it}"
        );
    }
    Ok(())
}

/// Run `bun` with `arguments` in `project_directory`.
fn run_bun(project_directory: &Path, arguments: &[&str]) -> Result<()> {
    let status = std::process::Command::new("bun")
        .args(arguments)
        .current_dir(project_directory)
        .status()
        .with_context(|| format!("running `bun {}`", arguments.join(" ")))?;
    if !status.success() {
        bail!("`bun {}` failed with {status}", arguments.join(" "));
    }
    Ok(())
}

/// Install the page's dependencies exactly as `bun.lock` pins them, with the
/// pinned bun, into the project's `node_modules`.
pub fn install_the_watch_pages_locked_dependencies(workspace_root: &Path) -> Result<PathBuf> {
    let project_directory = watch_page_project_in_the_workspace(workspace_root);
    ensure_the_pinned_bun_is_on_the_path(&project_directory)?;
    run_bun(&project_directory, &["install", "--frozen-lockfile"])?;
    Ok(project_directory)
}

/// Build the page and stage it into `staged_watch_page_directory`, replacing
/// whatever it held.
pub fn stage_the_watch_page(
    workspace_root: &Path,
    staged_watch_page_directory: &Path,
) -> Result<()> {
    let project_directory = install_the_watch_pages_locked_dependencies(workspace_root)?;
    if staged_watch_page_directory.exists() {
        std::fs::remove_dir_all(staged_watch_page_directory)
            .with_context(|| format!("removing {}", staged_watch_page_directory.display()))?;
    }
    std::fs::create_dir_all(staged_watch_page_directory)
        .with_context(|| format!("creating {}", staged_watch_page_directory.display()))?;
    let staged_watch_page_directory_argument = staged_watch_page_directory.display().to_string();
    run_bun(
        &project_directory,
        &[
            "build",
            WATCH_PAGE_SCRIPT_FILE_NAME,
            "--target",
            "browser",
            "--minify",
            "--outdir",
            &staged_watch_page_directory_argument,
        ],
    )?;
    copy_into(
        &project_directory.join(WATCH_PAGE_ENTRY_FILE_NAME),
        staged_watch_page_directory,
    )?;
    for opus_decoder_polyfill_file in opus_decoder_polyfill_files_loaded_beside_the_bundle(
        &project_directory.join(OPUS_DECODER_POLYFILL_DIST_RELATIVE_TO_THE_PROJECT),
    )? {
        copy_into(&opus_decoder_polyfill_file, staged_watch_page_directory)?;
    }
    ensure_the_staged_watch_page_is_complete(staged_watch_page_directory)?;
    tracing::info!(
        "stage-watch-page: the preview page is staged at {}",
        staged_watch_page_directory.display()
    );
    Ok(())
}

/// Copy `file` into `directory` under its own name.
fn copy_into(file: &Path, directory: &Path) -> Result<()> {
    let file_name = file
        .file_name()
        .with_context(|| format!("{} names no file", file.display()))?;
    std::fs::copy(file, directory.join(file_name))
        .with_context(|| format!("copying {} into {}", file.display(), directory.display()))?;
    Ok(())
}

/// The suffix of the polyfill's WebAssembly loader, the first file libav.js
/// fetches from beside the bundle.
const OPUS_DECODER_POLYFILL_WASM_LOADER_SUFFIX: &str =
    OPUS_DECODER_POLYFILL_FILE_SUFFIXES_LOADED_BESIDE_THE_BUNDLE[0];

/// The one file per suffix in [`OPUS_DECODER_POLYFILL_FILE_SUFFIXES_LOADED_BESIDE_THE_BUNDLE`]
/// in the polyfill's `dist_directory`.
fn opus_decoder_polyfill_files_loaded_beside_the_bundle(
    dist_directory: &Path,
) -> Result<Vec<PathBuf>> {
    OPUS_DECODER_POLYFILL_FILE_SUFFIXES_LOADED_BESIDE_THE_BUNDLE
        .iter()
        .map(|suffix| the_one_opus_decoder_polyfill_file_ending_in(dist_directory, suffix))
        .collect()
}

/// The one file in the polyfill's `dist_directory` whose name ends `suffix`.
fn the_one_opus_decoder_polyfill_file_ending_in(
    dist_directory: &Path,
    suffix: &str,
) -> Result<PathBuf> {
    let mut matching = Vec::new();
    for entry in std::fs::read_dir(dist_directory).with_context(|| {
        format!(
            "reading the Opus decoder polyfill at {}",
            dist_directory.display()
        )
    })? {
        let file_name = entry
            .with_context(|| format!("reading {}", dist_directory.display()))?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if file_name.ends_with(suffix) {
            matching.push(file_name);
        }
    }
    match matching.as_slice() {
        [one] => Ok(dist_directory.join(one)),
        _ => bail!(
            "the Opus decoder polyfill at {} holds {} files ending `{suffix}`, not one: {matching:?}",
            dist_directory.display(),
            matching.len()
        ),
    }
}

/// The polyfill's WebAssembly loader in the installed `project_directory`,
/// the file whose leading comment block is libav.js's own licence notice.
pub fn opus_decoder_polyfill_wasm_loader_in_the_project(
    project_directory: &Path,
) -> Result<PathBuf> {
    the_one_opus_decoder_polyfill_file_ending_in(
        &project_directory.join(OPUS_DECODER_POLYFILL_DIST_RELATIVE_TO_THE_PROJECT),
        OPUS_DECODER_POLYFILL_WASM_LOADER_SUFFIX,
    )
}

/// Refuse a staged page whose entry file does not load the bundle from where
/// the loopback HTTP serves it, or that lacks the bundle or a polyfill file.
pub fn ensure_the_staged_watch_page_is_complete(staged_watch_page_directory: &Path) -> Result<()> {
    let entry_file_path = staged_watch_page_directory.join(WATCH_PAGE_ENTRY_FILE_NAME);
    let entry_file = std::fs::read_to_string(&entry_file_path).with_context(|| {
        format!(
            "the staged preview page has no entry file at {}",
            entry_file_path.display()
        )
    })?;
    if !entry_file.contains(&format!(
        "src=\"{WATCH_PAGE_BUNDLE_PATH_THE_ENTRY_FILE_LOADS}\""
    )) {
        bail!(
            "{} loads no script from {WATCH_PAGE_BUNDLE_PATH_THE_ENTRY_FILE_LOADS}, where the \
             bundle is served",
            entry_file_path.display()
        );
    }
    let bundle_path = staged_watch_page_directory.join(WATCH_PAGE_BUNDLE_FILE_NAME);
    if !bundle_path.is_file() {
        bail!(
            "the staged preview page has no bundle at {}",
            bundle_path.display()
        );
    }
    opus_decoder_polyfill_files_loaded_beside_the_bundle(staged_watch_page_directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A staged page holding `file_names`, each with a placeholder body, and
    /// the checked-in entry file when `with_the_entry_file`.
    fn a_staged_watch_page(file_names: &[&str], with_the_entry_file: bool) -> tempfile::TempDir {
        let staged_watch_page_directory = tempfile::tempdir().unwrap();
        for file_name in file_names {
            std::fs::write(staged_watch_page_directory.path().join(file_name), "staged").unwrap();
        }
        if with_the_entry_file {
            std::fs::write(
                staged_watch_page_directory
                    .path()
                    .join(WATCH_PAGE_ENTRY_FILE_NAME),
                include_str!("../../runtime/tatolab-watch/index.html"),
            )
            .unwrap();
        }
        staged_watch_page_directory
    }

    const EVERY_BUILT_FILE: [&str; 3] = [
        WATCH_PAGE_BUNDLE_FILE_NAME,
        "libav-6.10.9.0-opus-af.wasm.mjs",
        "libav-6.10.9.0-opus-af.wasm.wasm",
    ];

    #[test]
    fn a_page_staged_with_the_checked_in_entry_file_the_bundle_and_the_polyfill_is_complete() {
        let staged_watch_page_directory = a_staged_watch_page(&EVERY_BUILT_FILE, true);

        ensure_the_staged_watch_page_is_complete(staged_watch_page_directory.path())
            .expect("every file the page loads is staged");
    }

    #[test]
    fn a_page_missing_any_file_it_loads_is_refused_naming_it() {
        for (missing_file_name, what_the_refusal_names) in [
            (WATCH_PAGE_BUNDLE_FILE_NAME, WATCH_PAGE_BUNDLE_FILE_NAME),
            ("libav-6.10.9.0-opus-af.wasm.mjs", ".wasm.mjs"),
            ("libav-6.10.9.0-opus-af.wasm.wasm", ".wasm.wasm"),
        ] {
            let staged_file_names: Vec<&str> = EVERY_BUILT_FILE
                .into_iter()
                .filter(|file_name| *file_name != missing_file_name)
                .collect();
            let staged_watch_page_directory = a_staged_watch_page(&staged_file_names, true);

            let refusal =
                ensure_the_staged_watch_page_is_complete(staged_watch_page_directory.path())
                    .expect_err("a page missing a file is refused")
                    .to_string();

            assert!(
                refusal.contains(what_the_refusal_names),
                "{missing_file_name}: {refusal}"
            );
        }
    }

    #[test]
    fn a_page_with_no_entry_file_is_refused_naming_where_it_looked() {
        let staged_watch_page_directory = a_staged_watch_page(&EVERY_BUILT_FILE, false);

        let refusal = ensure_the_staged_watch_page_is_complete(staged_watch_page_directory.path())
            .expect_err("a page with no entry file is refused")
            .to_string();

        assert!(refusal.contains(WATCH_PAGE_ENTRY_FILE_NAME), "{refusal}");
    }

    #[test]
    fn an_entry_file_loading_the_bundle_by_a_relative_path_is_refused() {
        let staged_watch_page_directory = a_staged_watch_page(&EVERY_BUILT_FILE, false);
        std::fs::write(
            staged_watch_page_directory
                .path()
                .join(WATCH_PAGE_ENTRY_FILE_NAME),
            "<script type=\"module\" src=\"main.js\"></script>",
        )
        .unwrap();

        let refusal = ensure_the_staged_watch_page_is_complete(staged_watch_page_directory.path())
            .expect_err("a relative bundle path names no file below /watch/<address>")
            .to_string();

        assert!(
            refusal.contains(WATCH_PAGE_BUNDLE_PATH_THE_ENTRY_FILE_LOADS),
            "{refusal}"
        );
    }

    #[test]
    fn the_bundle_path_the_entry_file_loads_is_the_bundle_under_the_watch_route() {
        let watch_route_prefix_in_the_listener =
            include_str!("../../runtime/streamlib-api-server/src/loopback_http_listener.rs");
        assert!(
            watch_route_prefix_in_the_listener
                .contains("const WATCH_PAGE_ROUTE_PREFIX: &str = \"/watch\";")
        );
        assert_eq!(
            WATCH_PAGE_BUNDLE_PATH_THE_ENTRY_FILE_LOADS,
            format!("/watch/{WATCH_PAGE_BUNDLE_FILE_NAME}")
        );
    }

    #[test]
    fn the_checked_in_project_pins_bun_in_its_package_manager_field() {
        let pinned_bun_version = pinned_bun_version_from_package_json(include_str!(
            "../../runtime/tatolab-watch/package.json"
        ))
        .expect("the checked-in package.json pins bun");

        assert!(
            pinned_bun_version.split('.').count() == 3
                && pinned_bun_version
                    .split('.')
                    .all(|part| part.parse::<u32>().is_ok()),
            "{pinned_bun_version}"
        );
    }

    #[test]
    fn a_package_manager_other_than_bun_is_refused_naming_it() {
        let refusal = pinned_bun_version_from_package_json(r#"{"packageManager": "npm@10.0.0"}"#)
            .expect_err("npm is not bun")
            .to_string();

        assert!(refusal.contains("npm@10.0.0"), "{refusal}");
    }

    #[test]
    fn a_polyfill_build_holding_two_files_of_one_suffix_is_refused_rather_than_guessed_between() {
        let dist_directory = a_staged_watch_page(
            &[
                "libav-6.10.9.0-opus-af.wasm.mjs",
                "libav-6.11.0.0-opus-af.wasm.mjs",
                "libav-6.10.9.0-opus-af.wasm.wasm",
            ],
            false,
        );

        let refusal = opus_decoder_polyfill_files_loaded_beside_the_bundle(dist_directory.path())
            .expect_err("two loaders are refused")
            .to_string();

        assert!(refusal.contains(".wasm.mjs"), "{refusal}");
    }
}
