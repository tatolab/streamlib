// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! What a running `uv sync` of a project's `.venv` has done so far, read line by line from the
//! standard error of the uv the runtime unit carries. Off a terminal uv draws no progress bar:
//! it writes one line as it resolves, as each download over its size threshold starts and
//! ends, as each build starts and ends, and once per installed or removed package, so a sync
//! is followed by package, never by byte.

/// The phase a project's `uv sync` is in, by the last line it wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectVenvSyncPhase {
    /// Finding a Python and resolving the project's requirements.
    Resolving,
    /// Fetching a package or a Python.
    Downloading,
    /// Building a package from its source.
    Building,
    /// Every package is prepared and the venv is being changed.
    Installing,
    /// The venv holds every package the project requires.
    Installed,
}

impl ProjectVenvSyncPhase {
    /// The phase as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolving => "resolving",
            Self::Downloading => "downloading",
            Self::Building => "building",
            Self::Installing => "installing",
            Self::Installed => "installed",
        }
    }
}

/// What a package a sync names is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectVenvSyncPackageKind {
    /// A Python distribution the venv installs.
    Distribution,
    /// The Python interpreter the venv is made with, which uv fetches when the machine has none.
    PythonInterpreter,
}

impl ProjectVenvSyncPackageKind {
    /// The kind as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Distribution => "distribution",
            Self::PythonInterpreter => "python_interpreter",
        }
    }
}

/// Where one package a sync names stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectVenvSyncPackageState {
    /// Its download has started.
    Downloading,
    /// Its download has ended.
    Downloaded,
    /// Its build from source has started.
    Building,
    /// Its build has ended.
    Built,
    /// The venv holds it.
    Installed,
    /// The venv no longer holds it.
    Uninstalled,
}

impl ProjectVenvSyncPackageState {
    /// The state as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Downloaded => "downloaded",
            Self::Building => "building",
            Self::Built => "built",
            Self::Installed => "installed",
            Self::Uninstalled => "uninstalled",
        }
    }
}

/// One package a sync named, in the order it first named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectVenvSyncPackage {
    /// The distribution's name, or the Python build's, as uv writes it.
    pub name: String,
    /// The version installed or removed; `None` until uv names it.
    pub version: Option<String>,
    /// A distribution or the Python interpreter.
    pub kind: ProjectVenvSyncPackageKind,
    /// Where it stands.
    pub state: ProjectVenvSyncPackageState,
    /// The download's size as uv states it, to its one decimal of KiB, MiB or GiB; `None` for a
    /// package uv downloads without naming it or does not download.
    pub download_size_bytes: Option<u64>,
}

/// What one `uv sync` of a project's `.venv` has done so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectVenvSyncProgress {
    /// The phase its last line put it in.
    pub phase: ProjectVenvSyncPhase,
    /// How many packages its resolution named, the project's own among them; `None` until it
    /// resolved.
    pub packages_resolved: Option<u64>,
    /// How many packages it downloaded or built before installing; `None` until it prepared
    /// them.
    pub packages_prepared: Option<u64>,
    /// How many packages it installed; `None` until it installed them.
    pub packages_installed: Option<u64>,
    /// Each package it named by its download, build, install or removal, first named first.
    pub packages: Vec<ProjectVenvSyncPackage>,
}

impl Default for ProjectVenvSyncProgress {
    fn default() -> Self {
        Self {
            phase: ProjectVenvSyncPhase::Resolving,
            packages_resolved: None,
            packages_prepared: None,
            packages_installed: None,
            packages: Vec::new(),
        }
    }
}

/// The suffix uv writes after the name of a Python build it fetches.
const PYTHON_INTERPRETER_DOWNLOAD_SUFFIX: &str = " (download)";

impl ProjectVenvSyncProgress {
    /// How many named downloads have ended.
    pub fn downloads_done(&self) -> usize {
        self.packages
            .iter()
            .filter(|package| {
                package.download_size_bytes.is_some()
                    && package.state != ProjectVenvSyncPackageState::Downloading
            })
            .count()
    }

    /// How many named downloads have started.
    pub fn downloads_started(&self) -> usize {
        self.packages
            .iter()
            .filter(|package| package.download_size_bytes.is_some())
            .count()
    }

    /// Take in one line of uv's standard error; answers whether it changed the progress.
    pub fn take_in_a_uv_standard_error_line(&mut self, uv_standard_error_line: &str) -> bool {
        let line = uv_standard_error_line.trim();
        if let Some(download) = line.strip_prefix("Downloading ") {
            let Some((name_and_kind, size)) = download.rsplit_once(" (") else {
                return false;
            };
            let (name, kind) = the_name_and_kind_of(name_and_kind);
            let download_size_bytes = the_bytes_of_a_size_uv_writes(size.trim_end_matches(')'));
            self.phase = ProjectVenvSyncPhase::Downloading;
            let package = self.the_package_named(name, kind);
            package.state = ProjectVenvSyncPackageState::Downloading;
            package.download_size_bytes = download_size_bytes;
            return true;
        }
        if let Some(downloaded) = line.strip_prefix("Downloaded ") {
            let (name, kind) = the_name_and_kind_of(downloaded);
            self.the_package_named(name, kind).state = ProjectVenvSyncPackageState::Downloaded;
            return true;
        }
        if let Some(building) = line.strip_prefix("Building ") {
            self.phase = ProjectVenvSyncPhase::Building;
            self.the_package_named(
                the_distribution_name_of(building),
                ProjectVenvSyncPackageKind::Distribution,
            )
            .state = ProjectVenvSyncPackageState::Building;
            return true;
        }
        if let Some(built) = line.strip_prefix("Built ") {
            self.the_package_named(
                the_distribution_name_of(built),
                ProjectVenvSyncPackageKind::Distribution,
            )
            .state = ProjectVenvSyncPackageState::Built;
            return true;
        }
        if let Some(count) = the_package_count_after("Resolved ", line) {
            self.phase = ProjectVenvSyncPhase::Resolving;
            self.packages_resolved = Some(count);
            return true;
        }
        if let Some(count) = the_package_count_after("Prepared ", line) {
            self.phase = ProjectVenvSyncPhase::Installing;
            self.packages_prepared = Some(count);
            return true;
        }
        if the_package_count_after("Uninstalled ", line).is_some() {
            self.phase = ProjectVenvSyncPhase::Installing;
            return true;
        }
        if let Some(count) = the_package_count_after("Installed ", line) {
            self.phase = ProjectVenvSyncPhase::Installed;
            self.packages_installed = Some(count);
            return true;
        }
        if the_package_count_after("Audited ", line).is_some() {
            self.phase = ProjectVenvSyncPhase::Installed;
            self.packages_installed.get_or_insert(0);
            return true;
        }
        let (state, name_and_version) =
            if let Some(installed) = line.strip_prefix("+ ").or_else(|| line.strip_prefix("~ ")) {
                (ProjectVenvSyncPackageState::Installed, installed)
            } else if let Some(uninstalled) = line.strip_prefix("- ") {
                (ProjectVenvSyncPackageState::Uninstalled, uninstalled)
            } else {
                return false;
            };
        let name_and_version = name_and_version
            .split_once(" (")
            .map_or(name_and_version, |(name_and_version, _)| name_and_version);
        let Some((name, version)) = name_and_version.split_once("==") else {
            return false;
        };
        let package = self.the_package_named(name, ProjectVenvSyncPackageKind::Distribution);
        package.state = state;
        package.version = Some(version.to_owned());
        true
    }

    /// The package named `name`, added in `kind` when the sync has not named it yet.
    fn the_package_named(
        &mut self,
        name: &str,
        kind: ProjectVenvSyncPackageKind,
    ) -> &mut ProjectVenvSyncPackage {
        let index = match self
            .packages
            .iter()
            .position(|package| package.name == name)
        {
            Some(index) => index,
            None => {
                self.packages.push(ProjectVenvSyncPackage {
                    name: name.to_owned(),
                    version: None,
                    kind,
                    state: ProjectVenvSyncPackageState::Downloading,
                    download_size_bytes: None,
                });
                self.packages.len() - 1
            }
        };
        &mut self.packages[index]
    }
}

/// A download's name and kind: a Python build is written with a ` (download)` suffix.
fn the_name_and_kind_of(name_as_uv_writes_it: &str) -> (&str, ProjectVenvSyncPackageKind) {
    match name_as_uv_writes_it.strip_suffix(PYTHON_INTERPRETER_DOWNLOAD_SUFFIX) {
        Some(python_build) => (python_build, ProjectVenvSyncPackageKind::PythonInterpreter),
        None => (
            name_as_uv_writes_it,
            ProjectVenvSyncPackageKind::Distribution,
        ),
    }
}

/// The distribution a build line names: `<name> @ <url>` or `<name>==<version>`.
fn the_distribution_name_of(build_line_rest: &str) -> &str {
    let name = build_line_rest
        .split_once(" @ ")
        .map_or(build_line_rest, |(name, _)| name);
    name.split_once("==").map_or(name, |(name, _)| name).trim()
}

/// `N` of `<prefix>N package(s) in <duration>`.
fn the_package_count_after(prefix: &str, line: &str) -> Option<u64> {
    let (count, rest) = line.strip_prefix(prefix)?.split_once(' ')?;
    if !rest.starts_with("package") {
        return None;
    }
    count.parse().ok()
}

/// The bytes of a size as uv writes it, `15.9MiB`, `812.3KiB`, `1.2GiB` or `512B`.
fn the_bytes_of_a_size_uv_writes(size: &str) -> Option<u64> {
    let (number, bytes_per_unit) = [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)]
        .into_iter()
        .find_map(|(unit, bytes_per_unit)| {
            size.strip_suffix(unit)
                .map(|number| (number, bytes_per_unit))
        })
        .or_else(|| size.strip_suffix('B').map(|number| (number, 1)))?;
    let number: f64 = number.parse().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some((number * bytes_per_unit as f64).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What uv 0.13.1 writes to a standard error that is no terminal while it makes a venv
    /// holding numpy, pillow, six and packaging, fetching numpy and pillow.
    const A_SYNC_DOWNLOADING_NUMPY_AND_PILLOW: &str = "\
Using CPython 3.13.18 interpreter at: /usr/local/bin/python3
Creating virtual environment at: .venv
Resolved 8 packages in 776ms
Downloading numpy (15.9MiB)
Downloading pillow (6.8MiB)
 Downloaded pillow
 Downloaded numpy
Prepared 4 packages in 12.50s
Installed 4 packages in 8ms
 + numpy==2.5.4
 + packaging==26.3
 + pillow==13.0.0
 + six==1.17.0
";

    /// What uv 0.13.1 writes while it fetches the Python a `.python-version` pins and builds a
    /// path source.
    const A_SYNC_FETCHING_PYTHON_AND_BUILDING_A_PATH_SOURCE: &str = "\
Downloading cpython-3.12.15-linux-x86_64-gnu (download) (32.7MiB)
 Downloaded cpython-3.12.15-linux-x86_64-gnu (download)
Using CPython 3.12.15
Creating virtual environment at: .venv
Resolved 3 packages in 2ms
   Building libx @ file:///tmp/r/lib
      Built libx @ file:///tmp/r/lib
Prepared 1 package in 4.18s
Installed 2 packages in 0.48ms
 + libx==0.2 (from file:///tmp/r/lib)
 + six==1.17.0
";

    fn the_progress_after_each_line_of(standard_error: &str) -> Vec<ProjectVenvSyncProgress> {
        let mut progress = ProjectVenvSyncProgress::default();
        standard_error
            .lines()
            .map(|line| {
                progress.take_in_a_uv_standard_error_line(line);
                progress.clone()
            })
            .collect()
    }

    #[test]
    fn each_download_is_followed_from_its_start_to_its_install_with_its_size() {
        let after_each_line = the_progress_after_each_line_of(A_SYNC_DOWNLOADING_NUMPY_AND_PILLOW);

        let both_downloading = &after_each_line[4];
        assert_eq!(both_downloading.phase, ProjectVenvSyncPhase::Downloading);
        assert_eq!(both_downloading.packages_resolved, Some(8));
        assert_eq!(both_downloading.downloads_started(), 2);
        assert_eq!(both_downloading.downloads_done(), 0);
        assert_eq!(
            both_downloading.packages[0],
            ProjectVenvSyncPackage {
                name: "numpy".to_owned(),
                version: None,
                kind: ProjectVenvSyncPackageKind::Distribution,
                state: ProjectVenvSyncPackageState::Downloading,
                download_size_bytes: Some(16_672_358),
            }
        );

        let pillow_downloaded = &after_each_line[5];
        assert_eq!(pillow_downloaded.downloads_done(), 1);
        assert_eq!(
            pillow_downloaded.packages[1].state,
            ProjectVenvSyncPackageState::Downloaded
        );

        let prepared = &after_each_line[7];
        assert_eq!(prepared.phase, ProjectVenvSyncPhase::Installing);
        assert_eq!(prepared.packages_prepared, Some(4));
        assert_eq!(prepared.downloads_done(), 2);

        let synced = after_each_line.last().unwrap();
        assert_eq!(synced.phase, ProjectVenvSyncPhase::Installed);
        assert_eq!(synced.packages_installed, Some(4));
        assert_eq!(
            synced
                .packages
                .iter()
                .map(|package| (
                    package.name.as_str(),
                    package.version.as_deref(),
                    package.state
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "numpy",
                    Some("2.5.4"),
                    ProjectVenvSyncPackageState::Installed
                ),
                (
                    "pillow",
                    Some("13.0.0"),
                    ProjectVenvSyncPackageState::Installed
                ),
                (
                    "packaging",
                    Some("26.3"),
                    ProjectVenvSyncPackageState::Installed
                ),
                (
                    "six",
                    Some("1.17.0"),
                    ProjectVenvSyncPackageState::Installed
                ),
            ]
        );
        assert_eq!(synced.packages[0].download_size_bytes, Some(16_672_358));
        assert_eq!(synced.packages[2].download_size_bytes, None);
    }

    #[test]
    fn a_python_fetched_and_a_path_source_built_are_each_followed() {
        let after_each_line =
            the_progress_after_each_line_of(A_SYNC_FETCHING_PYTHON_AND_BUILDING_A_PATH_SOURCE);

        let python_downloading = &after_each_line[0];
        assert_eq!(python_downloading.phase, ProjectVenvSyncPhase::Downloading);
        assert_eq!(
            python_downloading.packages,
            [ProjectVenvSyncPackage {
                name: "cpython-3.12.15-linux-x86_64-gnu".to_owned(),
                version: None,
                kind: ProjectVenvSyncPackageKind::PythonInterpreter,
                state: ProjectVenvSyncPackageState::Downloading,
                download_size_bytes: Some(34_288_435),
            }]
        );
        assert_eq!(
            after_each_line[1].packages[0].state,
            ProjectVenvSyncPackageState::Downloaded
        );

        let building = &after_each_line[5];
        assert_eq!(building.phase, ProjectVenvSyncPhase::Building);
        assert_eq!(building.packages[1].name, "libx");
        assert_eq!(
            building.packages[1].state,
            ProjectVenvSyncPackageState::Building
        );
        assert_eq!(
            after_each_line[6].packages[1].state,
            ProjectVenvSyncPackageState::Built
        );

        let synced = after_each_line.last().unwrap();
        assert_eq!(synced.packages[1].version.as_deref(), Some("0.2"));
        assert_eq!(
            synced.packages[1].state,
            ProjectVenvSyncPackageState::Installed
        );
        assert_eq!(synced.packages_installed, Some(2));
    }

    #[test]
    fn a_removed_package_is_uninstalled_and_a_sync_with_nothing_to_install_is_installed() {
        let mut progress = ProjectVenvSyncProgress::default();
        for line in [
            "Resolved 3 packages in 1ms",
            "Uninstalled 1 package in 2ms",
            " - six==1.17.0",
            "Audited 2 packages in 0.1ms",
        ] {
            assert!(progress.take_in_a_uv_standard_error_line(line), "{line}");
        }
        assert_eq!(progress.phase, ProjectVenvSyncPhase::Installed);
        assert_eq!(progress.packages_installed, Some(0));
        assert_eq!(
            progress.packages[0].state,
            ProjectVenvSyncPackageState::Uninstalled
        );
    }

    #[test]
    fn a_line_uv_writes_for_no_package_changes_nothing() {
        let mut progress = ProjectVenvSyncProgress::default();
        for line in [
            "Using CPython 3.13.18 interpreter at: /usr/local/bin/python3",
            "Creating virtual environment at: .venv",
            "warning: `VIRTUAL_ENV=/x` does not match the project environment path `.venv`",
            "error: Failed to download `numpy==2.5.4`",
            "Resolved the dependencies",
        ] {
            assert!(!progress.take_in_a_uv_standard_error_line(line), "{line}");
        }
        assert_eq!(progress, ProjectVenvSyncProgress::default());
    }

    #[test]
    fn each_size_unit_uv_writes_is_read_in_bytes() {
        assert_eq!(the_bytes_of_a_size_uv_writes("512B"), Some(512));
        assert_eq!(the_bytes_of_a_size_uv_writes("1.5KiB"), Some(1_536));
        assert_eq!(the_bytes_of_a_size_uv_writes("2.0MiB"), Some(2_097_152));
        assert_eq!(the_bytes_of_a_size_uv_writes("1.2GiB"), Some(1_288_490_189));
        assert_eq!(the_bytes_of_a_size_uv_writes("lots"), None);
    }
}
