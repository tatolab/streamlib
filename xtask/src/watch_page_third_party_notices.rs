// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The preview page's section of the engine's `THIRD-PARTY-NOTICES.md`: every
//! npm package `bun.lock` pins into `runtime/tatolab-watch/`, read from the
//! installed `node_modules`, with the licence files it ships.
//!
//! A package that ships no licence file is covered by a notice this section
//! reproduces once: the licence files of its upstream repository, checked in
//! under `vendor/watch-page-notices/`, or the notice that opens libav.js's
//! WebAssembly build, which the page carries unmodified. A package covered by
//! neither is refused, so a new dependency cannot reach the page with no
//! notice.

use crate::generate_third_party_notices::fenced_licence_text_block;
use crate::stage_watch_page::{
    WATCH_PAGE_PROJECT_RELATIVE_TO_WORKSPACE, install_the_watch_pages_locked_dependencies,
    opus_decoder_polyfill_wasm_loader_in_the_project,
};
use anyhow::{Context, Result, bail};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// An upstream repository whose npm packages ship no licence file, with the
/// licence files its root carries, checked in.
struct UpstreamRepositoryWithItsLicenceFilesCheckedIn {
    /// The repository, as its packages name it.
    repository_url: &'static str,
    licence_files_relative_to_workspace: [&'static str; 2],
}

const UPSTREAM_REPOSITORIES_WITH_THEIR_LICENCE_FILES_CHECKED_IN:
    [UpstreamRepositoryWithItsLicenceFilesCheckedIn; 2] = [
    UpstreamRepositoryWithItsLicenceFilesCheckedIn {
        repository_url: "https://github.com/moq-dev/moq",
        licence_files_relative_to_workspace: [
            "vendor/watch-page-notices/moq-dev-moq/LICENSE-MIT",
            "vendor/watch-page-notices/moq-dev-moq/LICENSE-APACHE",
        ],
    },
    UpstreamRepositoryWithItsLicenceFilesCheckedIn {
        repository_url: "https://github.com/moq-dev/web-transport",
        licence_files_relative_to_workspace: [
            "vendor/watch-page-notices/moq-dev-web-transport/LICENSE-MIT",
            "vendor/watch-page-notices/moq-dev-web-transport/LICENSE-APACHE",
        ],
    },
];

/// The scope libav.js publishes its builds under.
const LIBAV_JS_PACKAGE_SCOPE: &str = "@libav.js/";

/// The licence that asks no notice be kept.
const ZERO_CLAUSE_BSD_LICENSE_IDENTIFIER: &str = "0BSD";

/// One package installed into the page's `node_modules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledWatchPagePackage {
    /// The npm name, scope included.
    pub name: String,
    pub version: String,
    /// The `license` field as written.
    pub license: String,
    /// The package's repository, as a browsable URL.
    pub repository_url: Option<String>,
    /// Each licence file the package ships beside its `package.json`.
    pub licence_files: Vec<LicenceFileShippedInThePackage>,
}

/// One licence file an installed package ships beside its `package.json`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LicenceFileShippedInThePackage {
    pub file_name: String,
    pub text: String,
}

/// Where the notice for a package shipping no licence file is reproduced.
#[derive(Clone, Copy)]
enum NoticeOfAPackageShippingNoLicenceFile {
    TheLicenceFilesOfItsRepository(&'static UpstreamRepositoryWithItsLicenceFilesCheckedIn),
    TheNoticeOpeningLibavJsWebAssemblyBuild,
    NoneAskedByZeroClauseBsd,
}

impl NoticeOfAPackageShippingNoLicenceFile {
    fn for_the_package(package: &InstalledWatchPagePackage) -> Result<Self> {
        if let Some(upstream_repository) = UPSTREAM_REPOSITORIES_WITH_THEIR_LICENCE_FILES_CHECKED_IN
            .iter()
            .find(|upstream_repository| {
                package.repository_url.as_deref() == Some(upstream_repository.repository_url)
            })
        {
            Ok(Self::TheLicenceFilesOfItsRepository(upstream_repository))
        } else if package.name.starts_with(LIBAV_JS_PACKAGE_SCOPE) {
            Ok(Self::TheNoticeOpeningLibavJsWebAssemblyBuild)
        } else if package.license == ZERO_CLAUSE_BSD_LICENSE_IDENTIFIER {
            Ok(Self::NoneAskedByZeroClauseBsd)
        } else {
            bail!(
                "the preview page's dependency {} {} ({}, {}) ships no licence file and no \
                 notice this section reproduces covers it; check its repository's licence files \
                 in under vendor/watch-page-notices/ and name them in \
                 UPSTREAM_REPOSITORIES_WITH_THEIR_LICENCE_FILES_CHECKED_IN",
                package.name,
                package.version,
                package.license,
                package
                    .repository_url
                    .as_deref()
                    .unwrap_or("no repository named")
            )
        }
    }

    fn sentence(self) -> String {
        match self {
            Self::TheLicenceFilesOfItsRepository(upstream_repository) => format!(
                "Ships no licence file; the licence files of <{}>, below, are its terms.",
                upstream_repository.repository_url
            ),
            Self::TheNoticeOpeningLibavJsWebAssemblyBuild => {
                "Ships no licence file; the notice opening libav.js's WebAssembly build, below, \
                 is its terms."
                    .to_string()
            }
            Self::NoneAskedByZeroClauseBsd => {
                "Ships no licence file; 0BSD asks for no notice, and its text is reproduced \
                 above under \"BSD Zero Clause License\"."
                    .to_string()
            }
        }
    }
}

/// Every package installed in the page's `node_modules_directory`, nested
/// installs included, sorted by name and version.
pub fn installed_watch_page_packages(
    node_modules_directory: &Path,
) -> Result<Vec<InstalledWatchPagePackage>> {
    let mut packages = Vec::new();
    collect_the_packages_installed_in(node_modules_directory, &mut packages)?;
    packages.sort_by(|left, right| (&left.name, &left.version).cmp(&(&right.name, &right.version)));
    packages.dedup();
    Ok(packages)
}

fn collect_the_packages_installed_in(
    node_modules_directory: &Path,
    packages: &mut Vec<InstalledWatchPagePackage>,
) -> Result<()> {
    for package_directory in package_directories_in(node_modules_directory)? {
        packages.push(read_the_installed_package(&package_directory)?);
        let nested_node_modules_directory = package_directory.join("node_modules");
        if nested_node_modules_directory.is_dir() {
            collect_the_packages_installed_in(&nested_node_modules_directory, packages)?;
        }
    }
    Ok(())
}

/// The package directories directly in `node_modules_directory`: each entry,
/// or each entry of a `@scope` directory; `.bin` and the like are not
/// packages.
fn package_directories_in(node_modules_directory: &Path) -> Result<Vec<PathBuf>> {
    let mut package_directories = Vec::new();
    for entry in sorted_directory_entries(node_modules_directory)? {
        let Some(entry_name) = entry
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if entry_name.starts_with('.') {
            continue;
        }
        let candidate_package_directories = if entry_name.starts_with('@') {
            sorted_directory_entries(&entry)?
        } else {
            vec![entry]
        };
        package_directories.extend(
            candidate_package_directories
                .into_iter()
                .filter(|package_directory| package_directory.join("package.json").is_file()),
        );
    }
    Ok(package_directories)
}

fn sorted_directory_entries(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in
        std::fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?
    {
        let entry_path = entry
            .with_context(|| format!("reading {}", directory.display()))?
            .path();
        if entry_path.is_dir() {
            entries.push(entry_path);
        }
    }
    entries.sort();
    Ok(entries)
}

/// A `package.json` member that is a string, or an object whose `object_key`
/// member is one — the two shapes npm accepts for `license` and `repository`.
fn string_or_object_member_string(
    member: Option<&serde_json::Value>,
    object_key: &str,
) -> Option<String> {
    match member? {
        serde_json::Value::String(member_string) => Some(member_string.clone()),
        serde_json::Value::Object(member_object) => member_object
            .get(object_key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn read_the_installed_package(package_directory: &Path) -> Result<InstalledWatchPagePackage> {
    let package_json_path = package_directory.join("package.json");
    let package: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&package_json_path)
            .with_context(|| format!("reading {}", package_json_path.display()))?,
    )
    .with_context(|| format!("parsing {}", package_json_path.display()))?;
    let package_json_string_field = |name: &str| {
        package
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("{} names no `{name}`", package_json_path.display()))
    };
    let license = string_or_object_member_string(package.get("license"), "type")
        .with_context(|| format!("{} names no `license`", package_json_path.display()))?;
    let repository = string_or_object_member_string(package.get("repository"), "url");
    Ok(InstalledWatchPagePackage {
        name: package_json_string_field("name")?,
        version: package_json_string_field("version")?,
        license,
        repository_url: repository.as_deref().map(browsable_repository_url),
        licence_files: licence_files_shipped_in(package_directory)?,
    })
}

/// A `repository` field as a browsable URL: `git+https://…/x.git` and
/// `git://…` as `https://…/x`, an `owner/name` shorthand as GitHub's.
fn browsable_repository_url(repository: &str) -> String {
    let repository = repository.strip_prefix("git+").unwrap_or(repository);
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    let repository = repository.strip_prefix("github:").unwrap_or(repository);
    if let Some(without_scheme) = repository.strip_prefix("git://") {
        format!("https://{without_scheme}")
    } else if repository.contains("://") {
        repository.to_string()
    } else {
        format!("https://github.com/{repository}")
    }
}

/// The licence files directly in `package_directory`: those named for a
/// licence, a copying grant, a notice or a copyright.
fn licence_files_shipped_in(
    package_directory: &Path,
) -> Result<Vec<LicenceFileShippedInThePackage>> {
    let mut licence_files = Vec::new();
    for entry in std::fs::read_dir(package_directory)
        .with_context(|| format!("reading {}", package_directory.display()))?
    {
        let path = entry
            .with_context(|| format!("reading {}", package_directory.display()))?
            .path();
        let Some(file_name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        let lowercase_file_name = file_name.to_lowercase();
        let names_a_licence = ["license", "licence", "copying", "notice"]
            .iter()
            .any(|prefix| lowercase_file_name.starts_with(prefix))
            || lowercase_file_name.contains("copyright");
        if names_a_licence && path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            licence_files.push(LicenceFileShippedInThePackage { file_name, text });
        }
    }
    licence_files.sort();
    Ok(licence_files)
}

/// The section rendered from `packages`, with the checked-in licence files
/// read from `workspace_root` and libav.js's notice from
/// `libav_js_webassembly_build_notice`.
pub fn render_the_watch_page_section(
    workspace_root: &Path,
    packages: &[InstalledWatchPagePackage],
    libav_js_webassembly_build_notice: &str,
) -> Result<String> {
    let mut section = format!(
        "## The preview page\n\nThe runtime unit's `lib/tatolab/watch/` is the preview page, \
         `{WATCH_PAGE_PROJECT_RELATIVE_TO_WORKSPACE}/`, bundled with bun from the npm packages \
         its `bun.lock` pins, beside libav.js's Opus decoder, which the bundle leaves external \
         and the page carries as its own unmodified files. Each package, with the licence files \
         it ships:\n\n"
    );
    let mut libav_js_version = None;
    let mut upstream_repository_urls_cited = Vec::new();
    for package in packages {
        write!(
            section,
            "### `{}` {}\n\nLicence: `{}`.",
            package.name, package.version, package.license
        )?;
        if let Some(repository_url) = &package.repository_url {
            write!(section, " Source: <{repository_url}>.")?;
        }
        section.push_str("\n\n");
        if package.licence_files.is_empty() {
            let notice = NoticeOfAPackageShippingNoLicenceFile::for_the_package(package)?;
            match notice {
                NoticeOfAPackageShippingNoLicenceFile::TheLicenceFilesOfItsRepository(
                    upstream_repository,
                ) => {
                    if !upstream_repository_urls_cited.contains(&upstream_repository.repository_url)
                    {
                        upstream_repository_urls_cited.push(upstream_repository.repository_url);
                    }
                }
                NoticeOfAPackageShippingNoLicenceFile::TheNoticeOpeningLibavJsWebAssemblyBuild => {
                    libav_js_version.get_or_insert_with(|| package.version.clone());
                }
                NoticeOfAPackageShippingNoLicenceFile::NoneAskedByZeroClauseBsd => {}
            }
            section.push_str(&notice.sentence());
            section.push_str("\n\n");
        }
        for licence_file in &package.licence_files {
            write!(section, "`{}`:\n\n", licence_file.file_name)?;
            section.push_str(&fenced_licence_text_block(&licence_file.text).with_context(
                || {
                    format!(
                        "fencing {} {}'s {}",
                        package.name, package.version, licence_file.file_name
                    )
                },
            )?);
            section.push('\n');
        }
    }

    for upstream_repository in UPSTREAM_REPOSITORIES_WITH_THEIR_LICENCE_FILES_CHECKED_IN
        .iter()
        .filter(|upstream_repository| {
            upstream_repository_urls_cited.contains(&upstream_repository.repository_url)
        })
    {
        write!(
            section,
            "### The licence files of <{}>\n\n",
            upstream_repository.repository_url
        )?;
        for licence_file_relative_to_workspace in
            upstream_repository.licence_files_relative_to_workspace
        {
            let licence_file_path = workspace_root.join(licence_file_relative_to_workspace);
            let licence_text = std::fs::read_to_string(&licence_file_path)
                .with_context(|| format!("reading {}", licence_file_path.display()))?;
            section.push_str(
                &fenced_licence_text_block(&licence_text)
                    .with_context(|| format!("fencing {licence_file_relative_to_workspace}"))?,
            );
            section.push('\n');
        }
    }

    let libav_js_version = libav_js_version.context(
        "no libav.js package is installed in the preview page, whose Opus decoder polyfill is one",
    )?;
    write!(
        section,
        "### libav.js's WebAssembly build\n\nThe notice opening \
         `libav-*-opus-af.wasm.mjs`, which the page carries unmodified beside \
         `libav-opus-af.mjs`, the entry module that loads it, and the `.wasm` it loads. Their \
         corresponding source, the FFmpeg and Opus they compile among it, is libav.js \
         {libav_js_version} at <https://github.com/Yahweasel/libav.js>.\n\n"
    )?;
    section.push_str(
        &fenced_licence_text_block(libav_js_webassembly_build_notice)
            .context("fencing libav.js's WebAssembly build notice")?,
    );
    Ok(section)
}

/// The notice opening libav.js's WebAssembly loader `source`: its `/*! … */`
/// block without the ` * ` rail, every other indentation kept, since the LGPL
/// text it reproduces is laid out by it.
fn libav_js_webassembly_build_notice(source: &str) -> Result<String> {
    let block = source
        .strip_prefix("/*!")
        .context("libav.js's WebAssembly build opens with no `/*!` notice")?;
    let (notice, _rest) = block
        .split_once("*/")
        .context("libav.js's WebAssembly build notice is never closed")?;
    let notice = notice
        .lines()
        .map(|line| {
            line.strip_prefix(" *")
                .map_or(line, |rest| rest.strip_prefix(' ').unwrap_or(rest))
        })
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::ensure!(
        notice.contains("GNU LESSER GENERAL PUBLIC LICENSE"),
        "libav.js's WebAssembly build notice carries no LGPL text, which its FFmpeg is under"
    );
    Ok(notice.trim().to_string())
}

/// Install the page's locked dependencies and render its section.
pub fn render_the_watch_pages_closure(workspace_root: &Path) -> Result<String> {
    let project_directory = install_the_watch_pages_locked_dependencies(workspace_root)?;
    let packages = installed_watch_page_packages(&project_directory.join("node_modules"))?;
    let wasm_loader_path = opus_decoder_polyfill_wasm_loader_in_the_project(&project_directory)?;
    let wasm_loader = std::fs::read_to_string(&wasm_loader_path)
        .with_context(|| format!("reading {}", wasm_loader_path.display()))?;
    render_the_watch_page_section(
        workspace_root,
        &packages,
        &libav_js_webassembly_build_notice(&wasm_loader)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask sits in the workspace")
            .to_path_buf()
    }

    fn an_installed_package(
        node_modules_directory: &Path,
        name: &str,
        package_json: &str,
        licence_files: &[(&str, &str)],
    ) {
        let package_directory = node_modules_directory.join(name);
        std::fs::create_dir_all(&package_directory).unwrap();
        std::fs::write(package_directory.join("package.json"), package_json).unwrap();
        for (licence_file_name, licence_text) in licence_files {
            std::fs::write(package_directory.join(licence_file_name), licence_text).unwrap();
        }
    }

    fn a_package(
        name: &str,
        license: &str,
        repository_url: Option<&str>,
    ) -> InstalledWatchPagePackage {
        InstalledWatchPagePackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            license: license.to_string(),
            repository_url: repository_url.map(str::to_owned),
            licence_files: Vec::new(),
        }
    }

    const A_LIBAV_JS_NOTICE: &str = "libav.js\n\nGNU LESSER GENERAL PUBLIC LICENSE\nCopyright";

    #[test]
    fn scoped_and_nested_packages_are_read_with_their_licence_files() {
        let node_modules_directory = tempfile::tempdir().unwrap();
        an_installed_package(
            node_modules_directory.path(),
            "@moq/watch",
            r#"{"name":"@moq/watch","version":"0.6.3","license":"(MIT OR Apache-2.0)","repository":{"type":"git","url":"git+https://github.com/moq-dev/moq.git"}}"#,
            &[],
        );
        an_installed_package(
            node_modules_directory.path(),
            "pako",
            r#"{"name":"pako","version":"3.0.2","license":"(MIT AND Zlib)","repository":"nodeca/pako"}"#,
            &[(
                "LICENSE",
                "(The MIT License)\nCopyright (C) 2014+ by Vitaly Puzrin",
            )],
        );
        an_installed_package(
            &node_modules_directory.path().join("pako/node_modules"),
            "tslib",
            r#"{"name":"tslib","version":"2.8.1","license":"0BSD"}"#,
            &[
                ("LICENSE.txt", "0BSD"),
                ("CopyrightNotice.txt", "Copyright (c) Microsoft"),
            ],
        );
        std::fs::create_dir_all(node_modules_directory.path().join(".bin")).unwrap();

        let packages = installed_watch_page_packages(node_modules_directory.path()).unwrap();

        let names: Vec<&str> = packages
            .iter()
            .map(|package| package.name.as_str())
            .collect();
        assert_eq!(names, ["@moq/watch", "pako", "tslib"]);
        assert_eq!(
            packages[0].repository_url.as_deref(),
            Some("https://github.com/moq-dev/moq")
        );
        assert_eq!(
            packages[1].repository_url.as_deref(),
            Some("https://github.com/nodeca/pako")
        );
        let tslib_licence_file_names: Vec<&str> = packages[2]
            .licence_files
            .iter()
            .map(|licence_file| licence_file.file_name.as_str())
            .collect();
        assert_eq!(
            tslib_licence_file_names,
            ["CopyrightNotice.txt", "LICENSE.txt"]
        );
    }

    #[test]
    fn a_package_shipping_no_licence_file_and_covered_by_no_notice_is_refused_naming_it() {
        let refusal = render_the_watch_page_section(
            &workspace_root(),
            &[a_package(
                "left-pad",
                "WTFPL",
                Some("https://github.com/stevemao/left-pad"),
            )],
            A_LIBAV_JS_NOTICE,
        )
        .expect_err("an uncovered package is refused")
        .to_string();

        assert!(refusal.contains("left-pad 1.0.0 (WTFPL, "), "{refusal}");
    }

    #[test]
    fn packages_shipping_no_licence_file_point_at_the_notice_that_covers_them() {
        let section = render_the_watch_page_section(
            &workspace_root(),
            &[
                a_package("@libav.js/variant-opus-af", "LGPL-2.1", None),
                a_package(
                    "@moq/net",
                    "(MIT OR Apache-2.0)",
                    Some("https://github.com/moq-dev/moq"),
                ),
                a_package(
                    "@moq/qmux",
                    "(MIT OR Apache-2.0)",
                    Some("https://github.com/moq-dev/web-transport"),
                ),
                a_package("polyfill", ZERO_CLAUSE_BSD_LICENSE_IDENTIFIER, None),
            ],
            A_LIBAV_JS_NOTICE,
        )
        .unwrap();

        for expected in [
            "the licence files of <https://github.com/moq-dev/moq>, below, are its terms",
            "the licence files of <https://github.com/moq-dev/web-transport>, below, are its terms",
            "### The licence files of <https://github.com/moq-dev/moq>",
            "### The licence files of <https://github.com/moq-dev/web-transport>",
            "Copyright (c) 2023 Luke Curley",
            "Apache License",
            "0BSD asks for no notice",
            "libav.js 1.0.0 at <https://github.com/Yahweasel/libav.js>",
            A_LIBAV_JS_NOTICE,
        ] {
            assert!(section.contains(expected), "{expected}");
        }
    }

    #[test]
    fn a_repositorys_licence_files_are_reproduced_only_when_a_package_cites_them() {
        let section = render_the_watch_page_section(
            &workspace_root(),
            &[a_package("@libav.js/variant-opus-af", "LGPL-2.1", None)],
            A_LIBAV_JS_NOTICE,
        )
        .unwrap();

        assert!(!section.contains("### The licence files of"), "{section}");
    }

    #[test]
    fn every_checked_in_licence_file_names_its_copyright_or_its_terms() {
        for upstream_repository in &UPSTREAM_REPOSITORIES_WITH_THEIR_LICENCE_FILES_CHECKED_IN {
            for licence_file_relative_to_workspace in
                upstream_repository.licence_files_relative_to_workspace
            {
                let licence_text = std::fs::read_to_string(
                    workspace_root().join(licence_file_relative_to_workspace),
                )
                .unwrap();
                assert!(
                    licence_text.contains("Copyright") || licence_text.contains("Apache License"),
                    "{licence_file_relative_to_workspace}"
                );
            }
        }
    }

    #[test]
    fn libav_js_notice_is_its_opening_block_without_the_comment_rail() {
        let notice = libav_js_webassembly_build_notice(
            "/*!\n * libav.js v6.10.9.0\n *\n *  GNU LESSER GENERAL PUBLIC LICENSE\n */\nvar LibAV;",
        )
        .unwrap();

        assert_eq!(
            notice,
            "libav.js v6.10.9.0\n\n GNU LESSER GENERAL PUBLIC LICENSE"
        );
    }

    #[test]
    fn a_libav_js_build_opening_with_no_notice_is_refused() {
        assert!(libav_js_webassembly_build_notice("var LibAV;").is_err());
    }

    #[test]
    fn a_licence_text_holding_a_three_backtick_code_sample_is_fenced_whole() {
        let fenced = fenced_licence_text_block("Usage:\n```\nnpm i\n```").unwrap();

        assert!(fenced.starts_with("````text\n"), "{fenced}");
        assert!(fenced.contains("```\nnpm i\n```"), "{fenced}");
    }

    #[test]
    fn the_libav_notice_keeps_the_lgpl_layout_the_c_header_extractor_trims() {
        let source =
            "/*!\n * ffmpeg:\n *\n *                   GNU LESSER GENERAL PUBLIC LICENSE\n */";

        let libav_notice = libav_js_webassembly_build_notice(source).unwrap();
        let c_header_notice =
            crate::generate_third_party_notices::leading_comment_block_notice(source).unwrap();

        assert!(
            libav_notice.contains("\n                  GNU LESSER"),
            "{libav_notice}"
        );
        assert!(
            c_header_notice.contains("\nGNU LESSER"),
            "{c_header_notice}"
        );
    }

    #[test]
    fn the_committed_engine_notices_carry_the_watch_pages_closure() {
        let committed_notices = include_str!("../../THIRD-PARTY-NOTICES.md");
        for expected in [
            "## The preview page",
            "### `@moq/watch` ",
            "### `@libav.js/variant-opus-af` ",
            "### libav.js's WebAssembly build",
            "GNU LESSER GENERAL PUBLIC LICENSE",
        ] {
            assert!(committed_notices.contains(expected), "{expected}");
        }
        if committed_notices.contains(
            NoticeOfAPackageShippingNoLicenceFile::NoneAskedByZeroClauseBsd
                .sentence()
                .as_str(),
        ) {
            assert!(
                committed_notices.contains("### BSD Zero Clause License"),
                "a 0BSD package points at a licence text the file no longer carries"
            );
        }
    }
}
