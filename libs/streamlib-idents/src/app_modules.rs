// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

use crate::archive::{ArchiveKind, extract_tar_gz_bytes_to_dir, extract_zip_bytes_to_dir, sniff_archive_kind};
use crate::ident::PackageRef;
use crate::lockfile::{
    Lockfile, LockfileEntry, LockfileSource, MODULES_LOCKFILE_NAME, read_lockfile,
    write_modules_lockfile,
};
use crate::manifest::Manifest;
use crate::resolver::content_hash_for_package_dir;
use crate::semver::SemVer;

/// Conventional per-app modules folder name, created beside the app's
/// [`MODULES_LOCKFILE_NAME`]. Packages land at `streamlib_modules/@org/name/`.
pub const APP_MODULES_DIR_NAME: &str = "streamlib_modules";

/// Prefix reserved for in-flight staging entries inside the modules folder.
/// Readers of `streamlib_modules/` must ignore entries with this prefix.
pub const APP_MODULES_STAGING_PREFIX: &str = ".staging-";

/// Directory names skipped when materializing a folder source into
/// `streamlib_modules/` — build scratch and VCS metadata, never package
/// content.
const FOLDER_COPY_EXCLUDED_DIR_NAMES: &[&str] = &[".git", "target"];

/// A byte source `streamlib add` accepts: a local folder, a local archive
/// file, or a URL. Never a registry coordinate — the primitive is "here are
/// the bytes", not "resolve this against a registry".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddPackageSource {
    /// A directory containing `streamlib.yaml` plus package contents.
    Folder { path: PathBuf },
    /// A local archive file (`.slpkg` / `.zip` / `.tar.gz`); the container
    /// format is detected from magic bytes, not the extension.
    Archive { path: PathBuf },
    /// A `file://` / `http://` / `https://` URL to an archive.
    Url { url: String },
}

impl AddPackageSource {
    /// Classify a CLI-style spec string into a source flavor. On-disk paths
    /// win; URL schemes are matched next; `@`-prefixed non-path specs get the
    /// registry-coordinate guidance error.
    pub fn detect(spec: &str) -> Result<Self, AppModulesError> {
        if let Some((scheme, _rest)) = spec.split_once("://") {
            return match scheme {
                "file" | "http" | "https" => Ok(Self::Url {
                    url: spec.to_string(),
                }),
                other => Err(AppModulesError::UnsupportedSource {
                    spec: spec.to_string(),
                    detail: format!(
                        "unsupported URL scheme '{other}://' (expected file://, http://, or https://)"
                    ),
                }),
            };
        }
        let path = Path::new(spec);
        if path.is_dir() {
            return Ok(Self::Folder {
                path: path.to_path_buf(),
            });
        }
        if path.is_file() {
            return Ok(Self::Archive {
                path: path.to_path_buf(),
            });
        }
        if spec.starts_with('@') {
            return Err(AppModulesError::RegistryCoordinateNotASource {
                spec: spec.to_string(),
            });
        }
        Err(AppModulesError::SourceNotFound {
            spec: spec.to_string(),
        })
    }
}

/// Knobs for [`AppModulesDir::add_package`].
#[derive(Debug, Clone, Default)]
pub struct AddPackageOptions {
    /// Expected SHA-256 of the archive bytes (hex, optional `sha256:` prefix).
    /// When `Some`, a mismatch is a typed [`AppModulesError::HashMismatch`]
    /// and nothing is materialized. Ignored for folder sources (no archive
    /// bytes exist).
    pub expected_archive_sha256: Option<String>,
}

/// Outcome of a successful [`AppModulesDir::add_package`].
#[derive(Debug, Clone)]
pub struct AddPackageReport {
    /// The canonical `@org/name`, read from the package's own manifest.
    pub package: PackageRef,
    /// The version declared by the package's own manifest.
    pub version: SemVer,
    /// Where the package contents now live (`streamlib_modules/@org/name/`).
    pub package_dir: PathBuf,
    /// The modules lockfile that was updated.
    pub lockfile_path: PathBuf,
    /// Content hash recorded in the lockfile entry.
    pub content_hash: String,
    /// The source recorded in the lockfile entry.
    pub source: LockfileSource,
    /// `true` when an existing `streamlib_modules/@org/name/` was replaced.
    pub replaced_existing: bool,
}

/// Outcome of a successful [`AppModulesDir::remove_package`].
#[derive(Debug, Clone)]
pub struct RemovePackageReport {
    /// The canonical `@org/name` that was removed.
    pub package: PackageRef,
    /// The version the removed lockfile entry recorded; `None` when only an
    /// orphan folder (no lockfile entry) was removed.
    pub version: Option<SemVer>,
    /// The `streamlib_modules/@org/name/` path that was targeted.
    pub package_dir: PathBuf,
    /// `true` when the package folder existed on disk and was deleted.
    pub package_dir_removed: bool,
    /// `true` when a lockfile entry existed and was removed.
    pub lockfile_entry_removed: bool,
}

/// Outcome of a successful [`AppModulesDir::link_package`].
#[derive(Debug, Clone)]
pub struct LinkPackageReport {
    /// The canonical `@org/name`, read from the linked checkout's manifest.
    pub package: PackageRef,
    /// The version declared by the linked checkout's manifest at link time.
    pub version: SemVer,
    /// The `streamlib_modules/@org/name` symlink that now points at the
    /// checkout.
    pub package_dir: PathBuf,
    /// The canonical checkout path the symlink targets.
    pub link_target: PathBuf,
    /// The modules lockfile that was updated.
    pub lockfile_path: PathBuf,
    /// Content hash of the checkout at link time — a point-in-time snapshot,
    /// since a linked checkout's contents are live and may drift afterward.
    pub content_hash: String,
    /// The source recorded in the lockfile entry ([`LockfileSource::Link`]).
    pub source: LockfileSource,
    /// `true` when an existing `streamlib_modules/@org/name` slot (a prior
    /// link or an added copy) was replaced.
    pub replaced_existing: bool,
}

/// Outcome of a successful [`AppModulesDir::unlink_package`].
#[derive(Debug, Clone)]
pub struct UnlinkPackageReport {
    /// The canonical `@org/name` that was unlinked.
    pub package: PackageRef,
    /// The `streamlib_modules/@org/name` slot that was targeted.
    pub package_dir: PathBuf,
    /// The checkout the removed link pointed at, when the dropped lockfile
    /// entry recorded a [`LockfileSource::Link`].
    pub link_target: Option<PathBuf>,
    /// `true` when the symlink existed on disk and was removed.
    pub link_removed: bool,
    /// `true` when a lockfile entry existed and was removed.
    pub lockfile_entry_removed: bool,
}

/// How one lockfile entry was reproduced by
/// [`AppModulesDir::install_from_lockfile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledFromLockKind {
    /// Contents were copied / extracted from the recorded byte source (a
    /// [`LockfileSource::Path`] / [`LockfileSource::Archive`] /
    /// [`LockfileSource::Url`]) and re-verified against the recorded
    /// `content_hash`.
    Materialized,
    /// A symlink was re-created to the recorded checkout (a
    /// [`LockfileSource::Link`]). Not content-hash-verified — a linked checkout
    /// is live and may have drifted since it was linked.
    Linked,
}

/// Per-package outcome inside an [`AppModulesDir::install_from_lockfile`] run.
#[derive(Debug, Clone)]
pub struct InstalledFromLockPackage {
    /// The canonical `@org/name` reproduced (from the lockfile map key).
    pub package: PackageRef,
    /// The version the lockfile entry pinned.
    pub version: SemVer,
    /// The `streamlib_modules/@org/name` slot that was reproduced.
    pub package_dir: PathBuf,
    /// The source the lockfile entry recorded.
    pub source: LockfileSource,
    /// The content hash the lockfile pinned (re-verified for a materialized
    /// source; a point-in-time snapshot for a linked source).
    pub content_hash: String,
    /// How the slot was reproduced.
    pub kind: InstalledFromLockKind,
    /// `true` when an existing `streamlib_modules/@org/name` slot was replaced.
    pub replaced_existing: bool,
}

/// Outcome of a successful [`AppModulesDir::install_from_lockfile`].
#[derive(Debug, Clone)]
pub struct InstallFromLockfileReport {
    /// The modules lockfile that was reproduced from.
    pub lockfile_path: PathBuf,
    /// The `streamlib_modules/` folder that was reproduced.
    pub modules_dir: PathBuf,
    /// Every package reproduced, in lockfile (sorted) order.
    pub packages: Vec<InstalledFromLockPackage>,
}

/// Per-failure-mode error from the per-app modules primitive.
#[derive(Debug, thiserror::Error)]
pub enum AppModulesError {
    /// The spec names nothing on disk.
    #[error("source '{spec}' not found on disk")]
    SourceNotFound { spec: String },

    /// The `streamlib link` path is not a directory. Link takes a local
    /// package checkout folder — archives and URLs are `streamlib add` only.
    #[error(
        "link path '{}' is not a directory — `streamlib link` takes a local package \
         checkout folder (use `streamlib add` for an archive or URL)",
        path.display()
    )]
    LinkPathNotADirectory { path: PathBuf },

    /// An `@org/name`-shaped spec was passed where a byte source is required.
    #[error(
        "'{spec}' looks like a registry coordinate, not a byte source. `streamlib add` \
         takes a folder, an archive (.slpkg / .zip / .tar.gz), or a URL — obtain the \
         package's bytes (from its publisher, or a registry tree's slpkg/ store) and \
         add those"
    )]
    RegistryCoordinateNotASource { spec: String },

    /// The spec's flavor is recognized but not supported (e.g. an unknown URL
    /// scheme).
    #[error("unsupported source '{spec}': {detail}")]
    UnsupportedSource { spec: String, detail: String },

    /// The bytes are not a recognized archive container.
    #[error(
        "'{source_label}' is not a recognized archive: {detail} \
         (expected a zip-shaped .slpkg/.zip or a gzip-compressed .tar.gz)"
    )]
    UnsupportedArchive { source_label: String, detail: String },

    /// Fetching a URL source's bytes failed.
    #[error("fetching '{url}' failed: {detail}")]
    FetchFailed { url: String, detail: String },

    /// Extracting the archive into the staging directory failed.
    #[error("extracting '{source_label}' failed: {detail}")]
    ExtractFailed { source_label: String, detail: String },

    /// The staged contents are not a valid streamlib package.
    #[error("'{source_label}' is not a valid streamlib package: {detail}")]
    InvalidPackage { source_label: String, detail: String },

    /// The staged package's manifest has no `package:` identity block.
    #[error(
        "'{source_label}' has no `package:` block in its streamlib.yaml — a package \
         added to streamlib_modules/ must declare its own @org/name@version identity"
    )]
    MissingPackageIdentity { source_label: String },

    /// The archive bytes do not match the expected SHA-256.
    #[error("sha256 mismatch for '{source_label}': expected {expected}, got {actual}")]
    HashMismatch {
        source_label: String,
        expected: String,
        actual: String,
    },

    /// Promoting the staged package into its `streamlib_modules/` slot failed.
    #[error("promoting the staged package into {} failed: {detail}", package_dir.display())]
    StagePromoteFailed {
        package_dir: PathBuf,
        detail: String,
    },

    /// Creating the symlink for a `streamlib link` into `streamlib_modules/`
    /// failed.
    #[error(
        "creating the link {} -> {} failed: {detail}",
        link_path.display(),
        target.display()
    )]
    SymlinkCreateFailed {
        link_path: PathBuf,
        target: PathBuf,
        detail: String,
    },

    /// Reading the modules lockfile failed (present but unreadable/corrupt).
    #[error("reading the modules lockfile at {} failed: {detail}", lockfile_path.display())]
    LockfileReadFailed {
        lockfile_path: PathBuf,
        detail: String,
    },

    /// Writing the modules lockfile failed.
    #[error("writing the modules lockfile at {} failed: {detail}", lockfile_path.display())]
    LockfileWriteFailed {
        lockfile_path: PathBuf,
        detail: String,
    },

    /// Nothing to remove: no lockfile entry and no package folder.
    #[error("'{package}' is not installed in {}", modules_dir.display())]
    NotInstalled {
        package: PackageRef,
        modules_dir: PathBuf,
    },

    /// `streamlib unlink` was called on a package that is not linked. When
    /// `present_as_added_copy` is `true` the slot holds an added (copied)
    /// package — remove it with `streamlib remove`, not `unlink`.
    #[error(
        "'{package}' is not linked in {}{}",
        modules_dir.display(),
        if *present_as_added_copy {
            " — it is present as an added copy; use `streamlib remove` to remove it"
        } else {
            ""
        }
    )]
    NotLinked {
        package: PackageRef,
        modules_dir: PathBuf,
        present_as_added_copy: bool,
    },

    /// Filesystem operation failed.
    #[error("io error at {}: {detail}", path.display())]
    Io { path: PathBuf, detail: String },

    /// `install_from_lockfile` found no `streamlib.lock` at the app root —
    /// there is nothing to reproduce a `streamlib_modules/` folder from.
    #[error(
        "no {} to install from at {} — `streamlib install` reproduces streamlib_modules/ from a \
         committed lockfile; run `streamlib add`/`streamlib link` to create one, or commit the \
         lockfile before installing",
        MODULES_LOCKFILE_NAME,
        lockfile_path.display()
    )]
    InstallLockfileMissing { lockfile_path: PathBuf },

    /// A lockfile map key is not a canonical `@org/name` reference.
    #[error("lockfile entry key '{key}' is not a canonical @org/name reference: {detail}")]
    InstallInvalidLockEntry { key: String, detail: String },

    /// The byte source a lockfile entry records is unavailable at install time
    /// — a `path:`/`archive:` file that is gone, or a `url:` unreachable
    /// offline. Names the package so the operator knows which entry to fix.
    #[error("cannot reproduce '{package}' — its recorded source is unavailable: {detail}")]
    InstallSourceUnavailable { package: PackageRef, detail: String },

    /// A `link:` entry's checkout target no longer exists. A linked dev
    /// checkout is inherently non-reproducible on another machine — add the
    /// package from a portable source (archive / URL) or restore the checkout
    /// before installing.
    #[error(
        "cannot reproduce linked package '{package}' — its checkout target {} no longer exists. \
         A `streamlib link` records a dev-only symlink to a local checkout, which is not \
         reproducible elsewhere; add the package from a portable source (archive or URL), or \
         restore the checkout, then re-install",
        target.display()
    )]
    InstallDanglingLinkTarget { package: PackageRef, target: PathBuf },

    /// The archive bytes a `url:`/`archive:` entry re-fetched/read do not match
    /// the `archive_sha256` the lockfile recorded — the source changed under a
    /// pinned entry.
    #[error(
        "archive bytes for '{package}' do not match the recorded archive_sha256: expected \
         {expected}, got {actual}"
    )]
    InstallArchiveHashMismatch {
        package: PackageRef,
        expected: String,
        actual: String,
    },

    /// The reproduced package's content hash does not match the `content_hash`
    /// the lockfile pinned — the reproduced contents differ from what was
    /// locked.
    #[error(
        "content hash for '{package}' does not match the lockfile: expected {expected}, got \
         {actual}"
    )]
    InstallContentHashMismatch {
        package: PackageRef,
        expected: String,
        actual: String,
    },

    /// A lockfile entry records a source kind `install_from_lockfile` cannot
    /// reproduce (a `registry:` / `git:` coordinate). The per-app modules
    /// lockfile is only written with reproducible byte sources by
    /// `streamlib add`/`streamlib link`.
    #[error(
        "cannot reproduce '{package}' — its recorded source kind '{kind}' is not reproducible by \
         install (only path/archive/url/link sources are). This lockfile was not written by \
         `streamlib add`/`streamlib link`"
    )]
    InstallUnsupportedSource { package: PackageRef, kind: String },

    /// Reproducing a package into its slot failed (extract / invalid contents /
    /// promote / symlink). Names the package plus the underlying detail.
    #[error("reproducing '{package}' failed: {detail}")]
    InstallReproduceFailed { package: PackageRef, detail: String },
}

/// A per-app `streamlib_modules/` folder plus its `streamlib.lock`, anchored
/// at an explicit app root — never a walk-up, never `STREAMLIB_HOME`.
#[derive(Debug, Clone)]
pub struct AppModulesDir {
    app_root: PathBuf,
}

impl AppModulesDir {
    /// Anchor at an explicit app root directory.
    pub fn at(app_root: impl Into<PathBuf>) -> Self {
        Self {
            app_root: app_root.into(),
        }
    }

    /// Anchor at the process working directory (exact-CWD, no walk-up).
    pub fn from_cwd() -> Result<Self, AppModulesError> {
        let cwd = std::env::current_dir().map_err(|e| AppModulesError::Io {
            path: PathBuf::from("."),
            detail: format!("resolving current working directory: {e}"),
        })?;
        Ok(Self::at(cwd))
    }

    /// The app root this instance is anchored at.
    pub fn app_root(&self) -> &Path {
        &self.app_root
    }

    /// `<app-root>/streamlib_modules`.
    pub fn modules_dir(&self) -> PathBuf {
        self.app_root.join(APP_MODULES_DIR_NAME)
    }

    /// `<app-root>/streamlib.lock` — the modules lockfile.
    pub fn lockfile_path(&self) -> PathBuf {
        self.app_root.join(MODULES_LOCKFILE_NAME)
    }

    /// `<app-root>/streamlib_modules/@org/name` — a package's slot.
    pub fn package_dir(&self, package: &PackageRef) -> PathBuf {
        self.modules_dir()
            .join(format!("@{}", package.org))
            .join(package.name.as_str())
    }

    /// Read the modules lockfile; an absent file is an empty lock.
    pub fn read_lockfile(&self) -> Result<Lockfile, AppModulesError> {
        let path = self.lockfile_path();
        if !path.exists() {
            return Ok(Lockfile {
                version: 1,
                packages: Default::default(),
            });
        }
        read_lockfile(&path).map_err(|e| AppModulesError::LockfileReadFailed {
            lockfile_path: path,
            detail: e.to_string(),
        })
    }

    /// Materialize one package source into `streamlib_modules/@org/name/` and
    /// record it in the modules lockfile. Identity is read from the package's
    /// own manifest; re-adding an already-present package replaces it cleanly.
    /// Never builds; never resolves against a registry.
    #[tracing::instrument(skip(self, options), fields(app_root = %self.app_root.display()))]
    pub fn add_package(
        &self,
        source: &AddPackageSource,
        options: &AddPackageOptions,
    ) -> Result<AddPackageReport, AppModulesError> {
        let modules_dir = self.modules_dir();
        std::fs::create_dir_all(&modules_dir).map_err(|e| AppModulesError::Io {
            path: modules_dir.clone(),
            detail: format!("creating modules dir: {e}"),
        })?;
        sweep_orphan_staging_entries(&modules_dir);

        // Stage: materialize the source's bytes into a `.staging-*` sibling of
        // the final slot (same filesystem ⇒ the promote is an atomic rename).
        let staging = StagingDir::create(&modules_dir)?;
        let (lock_source, source_label) = stage_source_contents(source, options, staging.path())?;

        // Validate: identity from the staged package's OWN manifest.
        let staged_package_root = locate_staged_package_root(staging.path(), source, &source_label)?;
        let manifest =
            Manifest::load(&staged_package_root).map_err(|e| AppModulesError::InvalidPackage {
                source_label: source_label.clone(),
                detail: e.to_string(),
            })?;
        let package_meta =
            manifest
                .package
                .as_ref()
                .ok_or_else(|| AppModulesError::MissingPackageIdentity {
                    source_label: source_label.clone(),
                })?;
        let package = PackageRef::new(package_meta.org.clone(), package_meta.name.clone());
        let version = package_meta.version;

        let content_hash = content_hash_for_package_dir(&staged_package_root).map_err(|e| {
            AppModulesError::InvalidPackage {
                source_label: source_label.clone(),
                detail: format!("hashing package contents: {e}"),
            }
        })?;

        // Promote: swap the staged root into `streamlib_modules/@org/name`,
        // keeping the displaced previous contents restorable until the new
        // contents are in place.
        let package_dir = self.package_dir(&package);
        let replaced_existing =
            promote_staged_package_root(&staged_package_root, &package_dir, &modules_dir)?;
        drop(staging); // best-effort cleanup of the (now-emptied) staging shell

        // Lock: read-modify-write the modules lockfile, atomically.
        let lockfile_path = self.lockfile_path();
        let mut lockfile = self.read_lockfile()?;
        lockfile.packages.insert(
            package.to_string(),
            LockfileEntry {
                version,
                source: lock_source.clone(),
                content_hash: content_hash.clone(),
            },
        );
        write_modules_lockfile(&lockfile_path, &lockfile).map_err(|e| {
            AppModulesError::LockfileWriteFailed {
                lockfile_path: lockfile_path.clone(),
                detail: e.to_string(),
            }
        })?;

        tracing::info!(
            package = %package,
            %version,
            dir = %package_dir.display(),
            replaced = replaced_existing,
            "add_package: materialized into streamlib_modules"
        );
        Ok(AddPackageReport {
            package,
            version,
            package_dir,
            lockfile_path,
            content_hash,
            source: lock_source,
            replaced_existing,
        })
    }

    /// Remove one package: delete `streamlib_modules/@org/name/` (folder
    /// first), then drop its lockfile entry. [`AppModulesError::NotInstalled`]
    /// when neither exists; an orphan folder without a lockfile entry (or a
    /// lockfile entry whose folder is already gone) is healed.
    #[tracing::instrument(skip(self), fields(app_root = %self.app_root.display(), package = %package))]
    pub fn remove_package(
        &self,
        package: &PackageRef,
    ) -> Result<RemovePackageReport, AppModulesError> {
        let modules_dir = self.modules_dir();
        if modules_dir.is_dir() {
            sweep_orphan_staging_entries(&modules_dir);
        }

        let mut lockfile = self.read_lockfile()?;
        let removed_entry = lockfile.packages.remove(&package.to_string());
        let package_dir = self.package_dir(package);
        // `symlink_metadata` detects a symlink slot (a linked package) too,
        // without following it — a linked slot is removable via `remove`, and
        // only the symlink is unlinked, never the linked checkout.
        let package_dir_exists = std::fs::symlink_metadata(&package_dir).is_ok();

        if removed_entry.is_none() && !package_dir_exists {
            return Err(AppModulesError::NotInstalled {
                package: package.clone(),
                modules_dir: self.modules_dir(),
            });
        }

        // Folder first, then lock (a crash between the two leaves a lockfile
        // entry pointing at a gone folder — the healed direction — rather
        // than an unrecorded folder).
        let package_dir_removed = if package_dir_exists {
            remove_dir_entry_all(&package_dir).map_err(|e| AppModulesError::Io {
                path: package_dir.clone(),
                detail: format!("removing package dir: {e}"),
            })?;
            // Hygiene: drop the `@org` parent when this was its last package.
            if let Some(org_dir) = package_dir.parent() {
                let _ = std::fs::remove_dir(org_dir);
            }
            true
        } else {
            false
        };

        let lockfile_entry_removed = removed_entry.is_some();
        if lockfile_entry_removed {
            let lockfile_path = self.lockfile_path();
            write_modules_lockfile(&lockfile_path, &lockfile).map_err(|e| {
                AppModulesError::LockfileWriteFailed {
                    lockfile_path,
                    detail: e.to_string(),
                }
            })?;
        }

        tracing::info!(
            package = %package,
            dir_removed = package_dir_removed,
            entry_removed = lockfile_entry_removed,
            "remove_package: uninstalled from streamlib_modules"
        );
        Ok(RemovePackageReport {
            package: package.clone(),
            version: removed_entry.map(|e| e.version),
            package_dir,
            package_dir_removed,
            lockfile_entry_removed,
        })
    }

    /// Symlink a local package checkout into `streamlib_modules/@org/name` and
    /// record it in the modules lockfile as a [`LockfileSource::Link`]. Same
    /// primitive as [`add_package`](Self::add_package) — "make this package
    /// present" — but materialized as a symlink instead of a copy, so edits in
    /// the checkout are live on the next run with no re-add. Identity is read
    /// from the checkout's own manifest; re-linking (or linking over an added
    /// copy) replaces the slot cleanly. Never builds.
    #[tracing::instrument(skip(self), fields(app_root = %self.app_root.display(), source = %source_folder.display()))]
    pub fn link_package(
        &self,
        source_folder: &Path,
    ) -> Result<LinkPackageReport, AppModulesError> {
        // Validate the source is a directory BEFORE touching the modules dir,
        // so a bad link path leaves zero filesystem residue.
        if !source_folder.is_dir() {
            return Err(AppModulesError::LinkPathNotADirectory {
                path: source_folder.to_path_buf(),
            });
        }
        let canonical = std::fs::canonicalize(source_folder).map_err(|e| AppModulesError::Io {
            path: source_folder.to_path_buf(),
            detail: format!("canonicalizing link source: {e}"),
        })?;
        let source_label = canonical.display().to_string();

        // Identity + version come from the checkout's OWN manifest — the caller
        // supplies only the path.
        let manifest =
            Manifest::load(&canonical).map_err(|e| AppModulesError::InvalidPackage {
                source_label: source_label.clone(),
                detail: e.to_string(),
            })?;
        let package_meta =
            manifest
                .package
                .as_ref()
                .ok_or_else(|| AppModulesError::MissingPackageIdentity {
                    source_label: source_label.clone(),
                })?;
        let package = PackageRef::new(package_meta.org.clone(), package_meta.name.clone());
        let version = package_meta.version;

        // Point-in-time content hash of the checkout. A linked checkout is
        // live, so this snapshots what was linked rather than pinning it.
        let content_hash = content_hash_for_package_dir(&canonical).map_err(|e| {
            AppModulesError::InvalidPackage {
                source_label: source_label.clone(),
                detail: format!("hashing checkout contents: {e}"),
            }
        })?;

        let modules_dir = self.modules_dir();
        std::fs::create_dir_all(&modules_dir).map_err(|e| AppModulesError::Io {
            path: modules_dir.clone(),
            detail: format!("creating modules dir: {e}"),
        })?;
        sweep_orphan_staging_entries(&modules_dir);

        // Stage the symlink beside the final slot, then atomically promote it
        // into `@org/name` (same displace-restore path add_package uses) so a
        // failed swap never leaves the slot empty and re-linking is clean.
        let staging = StagingSymlink::create(&modules_dir, &canonical)?;
        let package_dir = self.package_dir(&package);
        let replaced_existing =
            promote_staged_package_root(staging.path(), &package_dir, &modules_dir)?;
        drop(staging); // the symlink was renamed into place; nothing to clean

        let lock_source = LockfileSource::Link {
            path: canonical.clone(),
        };
        let lockfile_path = self.lockfile_path();
        let mut lockfile = self.read_lockfile()?;
        lockfile.packages.insert(
            package.to_string(),
            LockfileEntry {
                version,
                source: lock_source.clone(),
                content_hash: content_hash.clone(),
            },
        );
        write_modules_lockfile(&lockfile_path, &lockfile).map_err(|e| {
            AppModulesError::LockfileWriteFailed {
                lockfile_path: lockfile_path.clone(),
                detail: e.to_string(),
            }
        })?;

        tracing::info!(
            package = %package,
            %version,
            link = %package_dir.display(),
            target = %canonical.display(),
            replaced = replaced_existing,
            "link_package: symlinked checkout into streamlib_modules"
        );
        Ok(LinkPackageReport {
            package,
            version,
            package_dir,
            link_target: canonical,
            lockfile_path,
            content_hash,
            source: lock_source,
            replaced_existing,
        })
    }

    /// Reverse a [`link_package`](Self::link_package): remove the
    /// `streamlib_modules/@org/name` symlink (never following it into the
    /// checkout) and drop its lockfile entry, dropping the slot back to
    /// nothing. The linked checkout on disk is untouched.
    /// [`AppModulesError::NotLinked`] when the package is not currently
    /// linked — including when the slot holds an added copy (which
    /// `streamlib remove` handles instead).
    #[tracing::instrument(skip(self), fields(app_root = %self.app_root.display(), package = %package))]
    pub fn unlink_package(
        &self,
        package: &PackageRef,
    ) -> Result<UnlinkPackageReport, AppModulesError> {
        let modules_dir = self.modules_dir();
        if modules_dir.is_dir() {
            sweep_orphan_staging_entries(&modules_dir);
        }

        let package_dir = self.package_dir(package);
        // `symlink_metadata` does NOT follow the link, so a dangling link is
        // still detected as a symlink slot.
        let slot_meta = std::fs::symlink_metadata(&package_dir).ok();
        let slot_is_symlink = slot_meta
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let slot_is_real_dir = slot_meta
            .as_ref()
            .map(|m| m.file_type().is_dir())
            .unwrap_or(false);

        let mut lockfile = self.read_lockfile()?;
        let key = package.to_string();
        let entry_link_target = match lockfile.packages.get(&key).map(|e| &e.source) {
            Some(LockfileSource::Link { path }) => Some(path.clone()),
            _ => None,
        };

        // A package is "linked" iff its slot is a symlink OR its lock entry
        // records a Link source (the symlink may already be gone — healed).
        if !slot_is_symlink && entry_link_target.is_none() {
            return Err(AppModulesError::NotLinked {
                package: package.clone(),
                modules_dir: self.modules_dir(),
                present_as_added_copy: slot_is_real_dir,
            });
        }

        // Remove the symlink (or a dangling one's residue). `remove_dir_entry_all`
        // unlinks a symlink without following it, so the checkout is untouched.
        let link_removed = if slot_meta.is_some() {
            remove_dir_entry_all(&package_dir).map_err(|e| AppModulesError::Io {
                path: package_dir.clone(),
                detail: format!("removing link: {e}"),
            })?;
            // Hygiene: drop the `@org` parent when this was its last package.
            if let Some(org_dir) = package_dir.parent() {
                let _ = std::fs::remove_dir(org_dir);
            }
            true
        } else {
            false
        };

        let lockfile_entry_removed = lockfile.packages.remove(&key).is_some();
        if lockfile_entry_removed {
            let lockfile_path = self.lockfile_path();
            write_modules_lockfile(&lockfile_path, &lockfile).map_err(|e| {
                AppModulesError::LockfileWriteFailed {
                    lockfile_path,
                    detail: e.to_string(),
                }
            })?;
        }

        tracing::info!(
            package = %package,
            link_removed,
            entry_removed = lockfile_entry_removed,
            "unlink_package: removed link from streamlib_modules"
        );
        Ok(UnlinkPackageReport {
            package: package.clone(),
            package_dir,
            link_target: entry_link_target,
            link_removed,
            lockfile_entry_removed,
        })
    }

    /// Reproduce `streamlib_modules/` from the committed `streamlib.lock`,
    /// exactly as `add`/`link` recorded it — the container/CI preinstall story.
    /// Each byte-source entry ([`LockfileSource::Path`] /
    /// [`LockfileSource::Archive`] / [`LockfileSource::Url`]) is re-materialized
    /// and re-verified against its recorded `content_hash`; a
    /// [`LockfileSource::Link`] entry's symlink is re-created. Makes NO
    /// resolution decisions and never rewrites the lockfile — install
    /// reproduces a decision `add`/`link` already made, so a clean checkout
    /// carrying only the lockfile rebuilds the same folder.
    ///
    /// Per-package-atomic and fail-fast: each package is staged, verified
    /// (before promote), then atomically promoted, so a failed package leaves
    /// no partial slot; a failure stops the run with a typed error naming the
    /// package, and packages already reproduced (each valid and hash-verified)
    /// remain — re-running install completes the reproduction. Running install
    /// twice yields the same folder. `Path`/`Link` sources reproduce only where
    /// their recorded local paths exist; a portable install (another machine)
    /// relies on `Url`/`Archive` entries or a vendored folder.
    ///
    /// Reproduction is **additive**: it materializes every locked entry but does
    /// not prune `streamlib_modules/@org/name` slots present on disk yet absent
    /// from the lock. A from-scratch reproduction (the container/CI target)
    /// starts from an empty folder, so the result equals the locked set exactly;
    /// a re-install over a dirty folder is a superset, not a clean slate. Not
    /// pruning is deliberate — install is non-destructive to slots it doesn't
    /// own, so a not-yet-`add`ed work-in-progress folder is never deleted.
    #[tracing::instrument(skip(self), fields(app_root = %self.app_root.display()))]
    pub fn install_from_lockfile(&self) -> Result<InstallFromLockfileReport, AppModulesError> {
        let lockfile_path = self.lockfile_path();
        if !lockfile_path.exists() {
            return Err(AppModulesError::InstallLockfileMissing { lockfile_path });
        }
        let lockfile = self.read_lockfile()?;

        let modules_dir = self.modules_dir();
        std::fs::create_dir_all(&modules_dir).map_err(|e| AppModulesError::Io {
            path: modules_dir.clone(),
            detail: format!("creating modules dir: {e}"),
        })?;
        sweep_orphan_staging_entries(&modules_dir);

        let mut packages = Vec::with_capacity(lockfile.packages.len());
        for (key, entry) in &lockfile.packages {
            let package = parse_lockfile_package_key(key)?;
            let (kind, replaced_existing) =
                self.reproduce_locked_entry(&package, entry, &modules_dir)?;
            tracing::info!(package = %package, ?kind, "install_from_lockfile: reproduced package");
            packages.push(InstalledFromLockPackage {
                package: package.clone(),
                version: entry.version,
                package_dir: self.package_dir(&package),
                source: entry.source.clone(),
                content_hash: entry.content_hash.clone(),
                kind,
                replaced_existing,
            });
        }

        tracing::info!(
            lockfile = %lockfile_path.display(),
            packages = packages.len(),
            "install_from_lockfile: reproduced streamlib_modules from lockfile"
        );
        Ok(InstallFromLockfileReport {
            lockfile_path,
            modules_dir,
            packages,
        })
    }

    /// Reproduce one lockfile entry into its `streamlib_modules/@org/name` slot.
    /// Returns how it was reproduced and whether a previous slot was replaced.
    fn reproduce_locked_entry(
        &self,
        package: &PackageRef,
        entry: &LockfileEntry,
        modules_dir: &Path,
    ) -> Result<(InstalledFromLockKind, bool), AppModulesError> {
        let package_dir = self.package_dir(package);
        match &entry.source {
            LockfileSource::Link { path } => {
                // Re-create the symlink iff the checkout target still exists.
                // The recorded content hash is a point-in-time snapshot of a
                // live checkout, so it is deliberately NOT re-verified here.
                if !path.is_dir() {
                    return Err(AppModulesError::InstallDanglingLinkTarget {
                        package: package.clone(),
                        target: path.clone(),
                    });
                }
                let staging = StagingSymlink::create(modules_dir, path)?;
                let replaced = promote_staged_package_root(staging.path(), &package_dir, modules_dir)
                    .map_err(|e| map_stage_error_to_install(package, e))?;
                drop(staging);
                Ok((InstalledFromLockKind::Linked, replaced))
            }
            LockfileSource::Path { path } => reproduce_materialized_from_lock(
                package,
                AddPackageSource::Folder { path: path.clone() },
                None,
                entry,
                modules_dir,
                &package_dir,
            ),
            LockfileSource::Archive {
                path,
                archive_sha256,
            } => reproduce_materialized_from_lock(
                package,
                AddPackageSource::Archive { path: path.clone() },
                Some(archive_sha256.clone()),
                entry,
                modules_dir,
                &package_dir,
            ),
            LockfileSource::Url {
                url,
                archive_sha256,
            } => reproduce_materialized_from_lock(
                package,
                AddPackageSource::Url { url: url.clone() },
                Some(archive_sha256.clone()),
                entry,
                modules_dir,
                &package_dir,
            ),
            LockfileSource::Registry { .. } => Err(AppModulesError::InstallUnsupportedSource {
                package: package.clone(),
                kind: "registry".to_string(),
            }),
            LockfileSource::Git { .. } => Err(AppModulesError::InstallUnsupportedSource {
                package: package.clone(),
                kind: "git".to_string(),
            }),
        }
    }
}

/// Best-effort sweep of orphaned `.staging-*` entries in `modules_dir` —
/// residue from an add/remove that was `SIGKILL`ed mid-promote (a clean
/// error path removes its own staging via [`StagingDir`]'s `Drop`). Only
/// entries whose embedded pid is NOT the current process are removed, so a
/// concurrent same-process add's in-flight staging dir is never deleted;
/// cross-process concurrent adds to one app root are unsupported
/// (last-writer-wins), so sweeping another process's staging entry is
/// acceptable. A removal failure is logged and ignored — the sweep is
/// hygiene, never a correctness gate.
fn sweep_orphan_staging_entries(modules_dir: &Path) {
    let current_pid = std::process::id();
    let entries = match std::fs::read_dir(modules_dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let Some(rest) = file_name.strip_prefix(APP_MODULES_STAGING_PREFIX) else {
            continue;
        };
        // `.staging-<pid>-<seq>` (fresh add), `.staging-link-<pid>-<seq>`
        // (fresh link), or `.staging-replaced-<pid>-<nanos>` (a promote
        // backup); the pid is the first numeric field after stripping any
        // non-numeric flavor prefix. Both prefixes must be peeled or a
        // current-process `link-` staging entry parses its pid as "link" ⇒
        // `None` and the same-process guard below would wrongly sweep it.
        let rest = rest.strip_prefix("replaced-").unwrap_or(rest);
        let rest = rest.strip_prefix("link-").unwrap_or(rest);
        let embedded_pid: Option<u32> = rest.split('-').next().and_then(|s| s.parse().ok());
        // Never delete an entry owned by THIS live process — that would race a
        // concurrent same-process add. An unparseable pid is treated as an
        // orphan and swept.
        if embedded_pid == Some(current_pid) {
            continue;
        }
        let path = entry.path();
        // A `.staging-link-*` orphan is a symlink; `remove_dir_entry_all`
        // unlinks it without following into a linked checkout.
        if let Err(e) = remove_dir_entry_all(&path) {
            tracing::debug!(
                dir = %path.display(),
                error = %e,
                "sweep_orphan_staging_entries: failed to remove orphan staging dir"
            );
        }
    }
}

/// A `.staging-*` directory removed on drop (best-effort) unless already
/// promoted away.
struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    fn create(modules_dir: &Path) -> Result<Self, AppModulesError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);
        let path = modules_dir.join(format!(
            "{APP_MODULES_STAGING_PREFIX}{}-{}",
            std::process::id(),
            STAGE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).map_err(|e| AppModulesError::Io {
            path: path.clone(),
            detail: format!("creating staging dir: {e}"),
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Remove a `streamlib_modules/` entry and everything under it: a symlink is
/// unlinked (never followed into its target), a real directory is recursively
/// removed, a plain file is deleted, and an already-absent path is a no-op.
/// The symlink case is why `streamlib link` slots can't go through
/// `remove_dir_all` directly — that would refuse a symlink (or, worse on
/// older toolchains, delete the linked checkout's contents).
fn remove_dir_entry_all(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                // Symlink (to a dir or file) or a plain file: unlink the entry
                // itself without following it.
                std::fs::remove_file(path)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A `.staging-*` symlink pointing at a link target, removed on drop
/// (best-effort, unlink-not-follow) unless already promoted away.
struct StagingSymlink {
    path: PathBuf,
}

impl StagingSymlink {
    fn create(modules_dir: &Path, target: &Path) -> Result<Self, AppModulesError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);
        let path = modules_dir.join(format!(
            "{APP_MODULES_STAGING_PREFIX}link-{}-{}",
            std::process::id(),
            STAGE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::os::unix::fs::symlink(target, &path).map_err(|e| {
            AppModulesError::SymlinkCreateFailed {
                link_path: path.clone(),
                target: target.to_path_buf(),
                detail: e.to_string(),
            }
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingSymlink {
    fn drop(&mut self) {
        // `symlink_metadata` succeeds for a still-present (possibly dangling)
        // staging symlink; if the promote consumed it via rename, this is a
        // no-op.
        if std::fs::symlink_metadata(&self.path).is_ok() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Materialize the source's contents into `staging_dir` and return the
/// [`LockfileSource`] to record plus a human-readable source label.
fn stage_source_contents(
    source: &AddPackageSource,
    options: &AddPackageOptions,
    staging_dir: &Path,
) -> Result<(LockfileSource, String), AppModulesError> {
    match source {
        AddPackageSource::Folder { path } => {
            if !path.is_dir() {
                return Err(AppModulesError::SourceNotFound {
                    spec: path.display().to_string(),
                });
            }
            let canonical = std::fs::canonicalize(path).map_err(|e| AppModulesError::Io {
                path: path.clone(),
                detail: format!("canonicalizing source folder: {e}"),
            })?;
            let mut visited_source_dirs = std::collections::HashSet::new();
            visited_source_dirs.insert(canonical.clone());
            copy_folder_contents(&canonical, staging_dir, &mut visited_source_dirs)?;
            let label = canonical.display().to_string();
            Ok((LockfileSource::Path { path: canonical }, label))
        }
        AddPackageSource::Archive { path } => {
            let bytes = std::fs::read(path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    AppModulesError::SourceNotFound {
                        spec: path.display().to_string(),
                    }
                } else {
                    AppModulesError::Io {
                        path: path.clone(),
                        detail: format!("reading archive: {e}"),
                    }
                }
            })?;
            let label = path.display().to_string();
            let archive_sha256 = verify_and_hash_archive_bytes(&bytes, options, &label)?;
            extract_archive_bytes(&bytes, staging_dir, &label)?;
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            Ok((
                LockfileSource::Archive {
                    path: canonical,
                    archive_sha256,
                },
                label,
            ))
        }
        AddPackageSource::Url { url } => {
            let bytes = fetch_url_bytes(url)?;
            let archive_sha256 = verify_and_hash_archive_bytes(&bytes, options, url)?;
            extract_archive_bytes(&bytes, staging_dir, url)?;
            Ok((
                LockfileSource::Url {
                    url: url.clone(),
                    archive_sha256,
                },
                url.clone(),
            ))
        }
    }
}

/// Verify the archive bytes against an expected SHA-256 (when supplied) and
/// return the actual lowercase hex digest.
fn verify_and_hash_archive_bytes(
    bytes: &[u8],
    options: &AddPackageOptions,
    source_label: &str,
) -> Result<String, AppModulesError> {
    let actual = sha256_hex(bytes);
    if let Some(expected) = &options.expected_archive_sha256 {
        let expected_hex = expected
            .trim()
            .strip_prefix("sha256:")
            .unwrap_or(expected.trim());
        if !actual.eq_ignore_ascii_case(expected_hex) {
            return Err(AppModulesError::HashMismatch {
                source_label: source_label.to_string(),
                expected: expected_hex.to_string(),
                actual,
            });
        }
    }
    Ok(actual)
}

/// Sniff the archive container from magic bytes and extract into `dest_dir`.
fn extract_archive_bytes(
    bytes: &[u8],
    dest_dir: &Path,
    source_label: &str,
) -> Result<(), AppModulesError> {
    let kind =
        sniff_archive_kind(bytes).ok_or_else(|| AppModulesError::UnsupportedArchive {
            source_label: source_label.to_string(),
            detail: "unrecognized magic bytes".to_string(),
        })?;
    let result = match kind {
        ArchiveKind::Zip => extract_zip_bytes_to_dir(bytes, dest_dir, source_label),
        ArchiveKind::TarGz => extract_tar_gz_bytes_to_dir(bytes, dest_dir, source_label),
    };
    result.map_err(|e| AppModulesError::ExtractFailed {
        source_label: source_label.to_string(),
        detail: e.to_string(),
    })
}

/// Find the staged package root: the staging dir itself when it carries
/// `streamlib.yaml`, else — for archive-shaped sources whose contents nest
/// under a single top-level directory (`tar czf pkg.tar.gz my-package/`) —
/// that single directory. Anything else is not a valid package.
fn locate_staged_package_root(
    staging_dir: &Path,
    source: &AddPackageSource,
    source_label: &str,
) -> Result<PathBuf, AppModulesError> {
    if staging_dir.join(Manifest::FILE_NAME).is_file() {
        return Ok(staging_dir.to_path_buf());
    }
    // Single-top-level-dir tolerance applies to archives only; a folder
    // source is taken literally.
    if !matches!(source, AddPackageSource::Folder { .. }) {
        let entries: Vec<PathBuf> = std::fs::read_dir(staging_dir)
            .map_err(|e| AppModulesError::Io {
                path: staging_dir.to_path_buf(),
                detail: format!("listing staged contents: {e}"),
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        if let [single] = entries.as_slice()
            && single.is_dir()
            && single.join(Manifest::FILE_NAME).is_file()
        {
            return Ok(single.clone());
        }
    }
    Err(AppModulesError::InvalidPackage {
        source_label: source_label.to_string(),
        detail: format!("no {} at the package root", Manifest::FILE_NAME),
    })
}

/// Swap `staged_package_root` into `package_dir`, displacing any previous
/// contents restorably. Returns whether previous contents were replaced.
fn promote_staged_package_root(
    staged_package_root: &Path,
    package_dir: &Path,
    modules_dir: &Path,
) -> Result<bool, AppModulesError> {
    let promote_err = |detail: String| AppModulesError::StagePromoteFailed {
        package_dir: package_dir.to_path_buf(),
        detail,
    };

    if let Some(parent) = package_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| promote_err(format!("creating {}: {e}", parent.display())))?;
    }

    // Displace previous contents into a staging-prefixed sibling so a failed
    // swap can restore them instead of leaving the slot empty.
    // `symlink_metadata` (not `exists`) so a DANGLING symlink slot — a prior
    // link whose checkout was deleted — is still detected as an occupied slot
    // and reported as replaced, rather than slipping through as "absent".
    let displaced = if std::fs::symlink_metadata(package_dir).is_ok() {
        let backup = modules_dir.join(format!(
            "{APP_MODULES_STAGING_PREFIX}replaced-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::rename(package_dir, &backup)
            .map_err(|e| promote_err(format!("displacing previous contents: {e}")))?;
        Some(backup)
    } else {
        None
    };

    match std::fs::rename(staged_package_root, package_dir) {
        Ok(()) => {
            if let Some(backup) = displaced {
                // The displaced previous slot may be a symlink (a prior link
                // being replaced); unlink it rather than recursing into it.
                let _ = remove_dir_entry_all(&backup);
                Ok(true)
            } else {
                Ok(false)
            }
        }
        Err(e) => {
            if let Some(backup) = &displaced {
                let _ = std::fs::rename(backup, package_dir);
            }
            Err(promote_err(format!("renaming staged package into place: {e}")))
        }
    }
}

/// Recursively copy a folder source's contents into `dest_dir`, skipping
/// [`FOLDER_COPY_EXCLUDED_DIR_NAMES`] directories and staging residue.
/// `visited_source_dirs` is the canonicalized recursion *stack* (entries are
/// removed on the way back out), so a self- or ancestor-referential symlink
/// is a loud error instead of infinite recursion while a diamond (two
/// symlinks to the same external dir) still copies.
fn copy_folder_contents(
    source_dir: &Path,
    dest_dir: &Path,
    visited_source_dirs: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), AppModulesError> {
    let io_err = |path: &Path, detail: String| AppModulesError::Io {
        path: path.to_path_buf(),
        detail,
    };
    let entries = std::fs::read_dir(source_dir)
        .map_err(|e| io_err(source_dir, format!("listing source folder: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(source_dir, format!("listing source folder: {e}")))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let src = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| io_err(&src, format!("reading file type: {e}")))?;
        if file_type.is_dir()
            && (FOLDER_COPY_EXCLUDED_DIR_NAMES.contains(&name_str.as_ref())
                || name_str.starts_with(APP_MODULES_STAGING_PREFIX))
        {
            continue;
        }
        let dst = dest_dir.join(&name);
        if file_type.is_dir() {
            std::fs::create_dir_all(&dst)
                .map_err(|e| io_err(&dst, format!("creating directory: {e}")))?;
            copy_folder_contents(&src, &dst, visited_source_dirs)?;
        } else if file_type.is_symlink() {
            // Resolve symlinks to their contents — the materialized package
            // must be self-contained (no dangling links to the source tree).
            let target =
                std::fs::read_link(&src).map_err(|e| io_err(&src, format!("reading symlink: {e}")))?;
            let resolved = if target.is_absolute() {
                target
            } else {
                src.parent().unwrap_or(source_dir).join(target)
            };
            if resolved.is_dir() {
                let canonical_target = std::fs::canonicalize(&resolved)
                    .map_err(|e| io_err(&src, format!("resolving symlink target: {e}")))?;
                if !visited_source_dirs.insert(canonical_target.clone()) {
                    return Err(io_err(
                        &src,
                        "symlink cycle detected while copying the folder source".to_string(),
                    ));
                }
                std::fs::create_dir_all(&dst)
                    .map_err(|e| io_err(&dst, format!("creating directory: {e}")))?;
                let copy_result = copy_folder_contents(&resolved, &dst, visited_source_dirs);
                visited_source_dirs.remove(&canonical_target);
                copy_result?;
            } else {
                std::fs::copy(&resolved, &dst)
                    .map_err(|e| io_err(&src, format!("copying symlink target: {e}")))?;
            }
        } else {
            std::fs::copy(&src, &dst).map_err(|e| io_err(&src, format!("copying file: {e}")))?;
        }
    }
    Ok(())
}

/// Fetch the raw bytes of a URL source. `file://` reads from disk;
/// `http(s)://` performs a blocking GET; any other scheme is rejected loud.
fn fetch_url_bytes(url: &str) -> Result<Vec<u8>, AppModulesError> {
    let fetch_err = |detail: String| AppModulesError::FetchFailed {
        url: url.to_string(),
        detail,
    };
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).map_err(|e| fetch_err(format!("reading {path}: {e}")));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let response = ureq::get(url)
            .call()
            .map_err(|e| fetch_err(format!("HTTP request failed: {e}")))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
            .map_err(|e| fetch_err(format!("reading HTTP response body: {e}")))?;
        return Ok(bytes);
    }
    Err(AppModulesError::UnsupportedSource {
        spec: url.to_string(),
        detail: "unsupported URL scheme (expected file://, http://, or https://)".to_string(),
    })
}

/// Lowercase hex-encoded SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Stage a byte source recorded in the lockfile, verify it against the pinned
/// `content_hash` BEFORE promoting (so a mismatch leaves no partial slot),
/// then atomically promote it into the package's slot. Shares the exact
/// stage → locate → promote machinery `add_package` uses.
fn reproduce_materialized_from_lock(
    package: &PackageRef,
    source: AddPackageSource,
    expected_archive_sha256: Option<String>,
    entry: &LockfileEntry,
    modules_dir: &Path,
    package_dir: &Path,
) -> Result<(InstalledFromLockKind, bool), AppModulesError> {
    let staging = StagingDir::create(modules_dir)?;
    let options = AddPackageOptions {
        expected_archive_sha256,
    };
    let (_lock_source, source_label) = stage_source_contents(&source, &options, staging.path())
        .map_err(|e| map_stage_error_to_install(package, e))?;
    let staged_root = locate_staged_package_root(staging.path(), &source, &source_label)
        .map_err(|e| map_stage_error_to_install(package, e))?;

    // Verify the reproduced contents against the pinned content hash BEFORE
    // promoting, so a mismatch leaves no partial slot (the staging dir is swept
    // by `StagingDir`'s Drop on the early return).
    let actual = content_hash_for_package_dir(&staged_root).map_err(|e| {
        AppModulesError::InstallReproduceFailed {
            package: package.clone(),
            detail: format!("hashing reproduced contents: {e}"),
        }
    })?;
    if actual != entry.content_hash {
        return Err(AppModulesError::InstallContentHashMismatch {
            package: package.clone(),
            expected: entry.content_hash.clone(),
            actual,
        });
    }

    let replaced = promote_staged_package_root(&staged_root, package_dir, modules_dir)
        .map_err(|e| map_stage_error_to_install(package, e))?;
    drop(staging);
    Ok((InstalledFromLockKind::Materialized, replaced))
}

/// Parse a lockfile map key (`@org/name`) into a typed [`PackageRef`] via the
/// canonical deserialize path (there is no `PackageRef::parse` by design).
fn parse_lockfile_package_key(key: &str) -> Result<PackageRef, AppModulesError> {
    serde_yaml::from_value::<PackageRef>(serde_yaml::Value::String(key.to_string())).map_err(|e| {
        AppModulesError::InstallInvalidLockEntry {
            key: key.to_string(),
            detail: e.to_string(),
        }
    })
}

/// Map a staging/promote [`AppModulesError`] onto the install-flavored,
/// package-named variant so an install failure always names the offending
/// package.
fn map_stage_error_to_install(package: &PackageRef, err: AppModulesError) -> AppModulesError {
    match err {
        AppModulesError::SourceNotFound { spec } => AppModulesError::InstallSourceUnavailable {
            package: package.clone(),
            detail: format!("source not found: {spec}"),
        },
        AppModulesError::FetchFailed { url, detail } => AppModulesError::InstallSourceUnavailable {
            package: package.clone(),
            detail: format!("fetching '{url}' failed: {detail}"),
        },
        AppModulesError::HashMismatch {
            expected, actual, ..
        } => AppModulesError::InstallArchiveHashMismatch {
            package: package.clone(),
            expected,
            actual,
        },
        other => AppModulesError::InstallReproduceFailed {
            package: package.clone(),
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::{Org, Package};
    use std::io::Write;

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    fn manifest_yaml(org: &str, name: &str, version: &str) -> String {
        format!(
            "package:\n  org: {org}\n  name: {name}\n  version: {version}\n  \
             description: a test package\nschemas:\n  FooFrame:\n    file: schemas/foo_frame.yaml\n"
        )
    }

    const SCHEMA_YAML: &str = "metadata:\n  type: FooFrame\n  description: \"A demo frame\"\n\
                               properties:\n  width:\n    type: uint32\n";

    /// Write a minimal valid package folder (manifest + one owned schema).
    fn write_package_folder(dir: &Path, org: &str, name: &str, version: &str) {
        std::fs::create_dir_all(dir.join("schemas")).unwrap();
        std::fs::write(dir.join("streamlib.yaml"), manifest_yaml(org, name, version)).unwrap();
        std::fs::write(dir.join("schemas/foo_frame.yaml"), SCHEMA_YAML).unwrap();
    }

    /// Zip-shaped `.slpkg` bytes for a minimal package.
    fn slpkg_bytes(org: &str, name: &str, version: &str) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::<()>::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("streamlib.yaml", opts).unwrap();
            writer
                .write_all(manifest_yaml(org, name, version).as_bytes())
                .unwrap();
            writer.start_file("schemas/foo_frame.yaml", opts).unwrap();
            writer.write_all(SCHEMA_YAML.as_bytes()).unwrap();
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    /// `.tar.gz` bytes for a minimal package, optionally nested under a
    /// single top-level directory (the `tar czf pkg.tar.gz my-package/` shape).
    fn tar_gz_package_bytes(org: &str, name: &str, version: &str, nested_under: Option<&str>) -> Vec<u8> {
        let prefix = nested_under.map(|d| format!("{d}/")).unwrap_or_default();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (path, body) in [
            (
                format!("{prefix}streamlib.yaml"),
                manifest_yaml(org, name, version),
            ),
            (format!("{prefix}schemas/foo_frame.yaml"), SCHEMA_YAML.to_string()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// Assert the app root carries NO partial state: no package dirs beyond
    /// `expected_packages`, no `.staging-*` residue, and the lockfile bytes
    /// equal `expected_lock_bytes` (None ⇒ no lockfile).
    fn assert_no_partial_state(
        app: &AppModulesDir,
        expected_packages: &[&str],
        expected_lock_bytes: Option<&[u8]>,
    ) {
        let modules = app.modules_dir();
        if modules.is_dir() {
            let mut found = Vec::new();
            for org_entry in std::fs::read_dir(&modules).unwrap().flatten() {
                let name = org_entry.file_name().to_string_lossy().into_owned();
                assert!(
                    !name.starts_with(APP_MODULES_STAGING_PREFIX),
                    "staging residue: {name}"
                );
                if org_entry.path().is_dir() {
                    for pkg_entry in std::fs::read_dir(org_entry.path()).unwrap().flatten() {
                        found.push(format!(
                            "{name}/{}",
                            pkg_entry.file_name().to_string_lossy()
                        ));
                    }
                }
            }
            found.sort();
            let mut expected: Vec<String> =
                expected_packages.iter().map(|s| s.to_string()).collect();
            expected.sort();
            assert_eq!(found, expected, "unexpected package dirs");
        } else {
            assert!(expected_packages.is_empty());
        }
        match expected_lock_bytes {
            Some(bytes) => assert_eq!(
                std::fs::read(app.lockfile_path()).unwrap(),
                bytes,
                "lockfile bytes changed"
            ),
            None => assert!(
                !app.lockfile_path().exists(),
                "no lockfile may be written on a failed add"
            ),
        }
    }

    // =====================================================================
    // Source detection
    // =====================================================================

    #[test]
    fn detect_classifies_folder_archive_url_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("pkg");
        std::fs::create_dir_all(&folder).unwrap();
        let archive = dir.path().join("pkg.slpkg");
        std::fs::write(&archive, b"x").unwrap();

        assert_eq!(
            AddPackageSource::detect(folder.to_str().unwrap()).unwrap(),
            AddPackageSource::Folder {
                path: folder.clone()
            }
        );
        assert_eq!(
            AddPackageSource::detect(archive.to_str().unwrap()).unwrap(),
            AddPackageSource::Archive {
                path: archive.clone()
            }
        );
        assert_eq!(
            AddPackageSource::detect("https://example.com/pkg.slpkg").unwrap(),
            AddPackageSource::Url {
                url: "https://example.com/pkg.slpkg".into()
            }
        );
        assert_eq!(
            AddPackageSource::detect("file:///tmp/pkg.slpkg").unwrap(),
            AddPackageSource::Url {
                url: "file:///tmp/pkg.slpkg".into()
            }
        );
        assert!(matches!(
            AddPackageSource::detect("ftp://example.com/pkg.slpkg"),
            Err(AppModulesError::UnsupportedSource { .. })
        ));
        assert!(matches!(
            AddPackageSource::detect("@tatolab/camera"),
            Err(AppModulesError::RegistryCoordinateNotASource { .. })
        ));
        assert!(matches!(
            AddPackageSource::detect("@tatolab/camera@^2.0"),
            Err(AppModulesError::RegistryCoordinateNotASource { .. })
        ));
        assert!(matches!(
            AddPackageSource::detect("./does-not-exist"),
            Err(AppModulesError::SourceNotFound { .. })
        ));
    }

    #[test]
    fn detect_prefers_an_on_disk_path_over_the_at_prefix() {
        // A literal directory whose name starts with `@` is a folder source,
        // not a registry coordinate.
        let dir = tempfile::tempdir().unwrap();
        let at_dir = dir.path().join("@weird");
        std::fs::create_dir_all(&at_dir).unwrap();
        assert!(matches!(
            AddPackageSource::detect(at_dir.to_str().unwrap()).unwrap(),
            AddPackageSource::Folder { .. }
        ));
    }

    // =====================================================================
    // Positive adds — folder / zip / tar.gz / file URL
    // =====================================================================

    #[test]
    fn folder_add_materializes_and_locks() {
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());

        let report = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .expect("folder add must succeed");

        assert_eq!(report.package, pkg_ref("tatolab", "camera"));
        assert_eq!(report.version, SemVer::new(2, 0, 0));
        assert!(!report.replaced_existing);
        assert_eq!(report.package_dir, app.package_dir(&report.package));
        assert!(report.package_dir.join("streamlib.yaml").is_file());
        assert!(report.package_dir.join("schemas/foo_frame.yaml").is_file());
        assert!(matches!(report.source, LockfileSource::Path { .. }));

        // The lock entry matches the report AND the on-disk contents.
        let lock = app.read_lockfile().unwrap();
        let entry = lock.packages.get("@tatolab/camera").expect("locked");
        assert_eq!(entry.version, SemVer::new(2, 0, 0));
        assert_eq!(entry.content_hash, report.content_hash);
        assert_eq!(
            entry.content_hash,
            content_hash_for_package_dir(&report.package_dir).unwrap(),
            "lock content_hash must equal the final dir's re-hash"
        );
        assert_no_partial_state(
            &app,
            &["@tatolab/camera"],
            Some(&std::fs::read(app.lockfile_path()).unwrap()),
        );
    }

    #[test]
    fn folder_add_skips_vcs_and_build_scratch_dirs() {
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        std::fs::create_dir_all(src.path().join(".git")).unwrap();
        std::fs::write(src.path().join(".git/HEAD"), b"ref").unwrap();
        std::fs::create_dir_all(src.path().join("target/debug")).unwrap();
        std::fs::write(src.path().join("target/debug/scratch.o"), b"obj").unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();
        assert!(!report.package_dir.join(".git").exists());
        assert!(!report.package_dir.join("target").exists());
        assert!(report.package_dir.join("streamlib.yaml").is_file());
    }

    #[test]
    fn folder_add_with_symlink_cycle_errors_instead_of_recursing() {
        // A self-referential symlink inside the source folder must be a loud
        // error, not infinite recursion. Mentally revert the visited-stack
        // guard in `copy_folder_contents` and this test never terminates.
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        std::os::unix::fs::symlink(src.path(), src.path().join("loop")).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .expect_err("a symlink cycle must fail loud");
        assert!(
            err.to_string().contains("symlink cycle"),
            "expected a symlink-cycle error, got {err:?}"
        );
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn folder_add_follows_non_cyclic_symlinks_into_contents() {
        // A benign symlink (file + external dir) is resolved into real
        // contents — the materialized package is self-contained.
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("extra.txt"), b"extra").unwrap();
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        std::os::unix::fs::symlink(
            external.path().join("extra.txt"),
            src.path().join("linked.txt"),
        )
        .unwrap();
        std::os::unix::fs::symlink(external.path(), src.path().join("linked_dir")).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .expect("benign symlinks must copy through");
        assert_eq!(
            std::fs::read(report.package_dir.join("linked.txt")).unwrap(),
            b"extra"
        );
        assert_eq!(
            std::fs::read(report.package_dir.join("linked_dir/extra.txt")).unwrap(),
            b"extra"
        );
        assert!(
            !report
                .package_dir
                .join("linked_dir")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "materialized contents must be real files, not links"
        );
    }

    #[test]
    fn slpkg_zip_add_materializes_and_locks_archive_sha() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("camera.slpkg");
        let bytes = slpkg_bytes("tatolab", "camera", "2.0.0");
        std::fs::write(&archive, &bytes).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Archive {
                    path: archive.clone(),
                },
                &AddPackageOptions::default(),
            )
            .expect("slpkg add must succeed");

        assert_eq!(report.package, pkg_ref("tatolab", "camera"));
        assert!(report.package_dir.join("streamlib.yaml").is_file());
        match &report.source {
            LockfileSource::Archive {
                archive_sha256, ..
            } => assert_eq!(archive_sha256, &sha256_hex(&bytes)),
            other => panic!("expected Archive source, got {other:?}"),
        }
        let lock = app.read_lockfile().unwrap();
        assert!(lock.packages.contains_key("@tatolab/camera"));
    }

    #[test]
    fn tar_gz_add_materializes_including_nested_single_dir_shape() {
        let dir = tempfile::tempdir().unwrap();
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());

        // Flat shape: manifest at the archive root.
        let flat = dir.path().join("flat.tar.gz");
        std::fs::write(&flat, tar_gz_package_bytes("tatolab", "camera", "2.0.0", None)).unwrap();
        let report = app
            .add_package(
                &AddPackageSource::Archive { path: flat },
                &AddPackageOptions::default(),
            )
            .expect("flat tar.gz add must succeed");
        assert_eq!(report.package, pkg_ref("tatolab", "camera"));

        // Nested shape: `tar czf pkg.tar.gz my-package/`.
        let nested = dir.path().join("nested.tar.gz");
        std::fs::write(
            &nested,
            tar_gz_package_bytes("tatolab", "mic", "1.0.0", Some("my-package")),
        )
        .unwrap();
        let report = app
            .add_package(
                &AddPackageSource::Archive { path: nested },
                &AddPackageOptions::default(),
            )
            .expect("nested tar.gz add must succeed");
        assert_eq!(report.package, pkg_ref("tatolab", "mic"));
        assert!(
            report.package_dir.join("streamlib.yaml").is_file(),
            "nested package root must be lifted to the slot root"
        );
        assert_no_partial_state(
            &app,
            &["@tatolab/camera", "@tatolab/mic"],
            Some(&std::fs::read(app.lockfile_path()).unwrap()),
        );
    }

    #[test]
    fn file_url_add_materializes_and_locks_url_source() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("camera.slpkg");
        let bytes = slpkg_bytes("tatolab", "camera", "2.0.0");
        std::fs::write(&archive, &bytes).unwrap();
        let url = format!("file://{}", archive.display());

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Url { url: url.clone() },
                &AddPackageOptions::default(),
            )
            .expect("file:// add must succeed");
        match &report.source {
            LockfileSource::Url {
                url: recorded,
                archive_sha256,
            } => {
                assert_eq!(recorded, &url);
                assert_eq!(archive_sha256, &sha256_hex(&bytes));
            }
            other => panic!("expected Url source, got {other:?}"),
        }
    }

    /// The blocking `http://` path downloads from a one-shot localhost
    /// server (mirrors the engine's `fetch_http_url_downloads_bytes`).
    #[test]
    fn http_url_add_downloads_and_materializes() {
        use std::io::Read;

        let body = slpkg_bytes("tatolab", "camera", "2.0.0");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body_for_server = body.clone();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body_for_server.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
                let _ = std::io::Write::write_all(&mut stream, &body_for_server);
                let _ = std::io::Write::flush(&mut stream);
            }
        });

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let url = format!("http://127.0.0.1:{port}/camera.slpkg");
        let report = app
            .add_package(
                &AddPackageSource::Url { url },
                &AddPackageOptions::default(),
            )
            .expect("http add must succeed");
        assert_eq!(report.package, pkg_ref("tatolab", "camera"));
        assert!(report.package_dir.join("streamlib.yaml").is_file());
        server.join().unwrap();
    }

    // =====================================================================
    // Identity from manifest, idempotent re-add, version upgrade
    // =====================================================================

    #[test]
    fn identity_comes_from_the_manifest_not_the_source_name() {
        // A dir named `weird-dir` declaring @tatolab/camera lands at
        // streamlib_modules/@tatolab/camera.
        let parent = tempfile::tempdir().unwrap();
        let weird = parent.path().join("weird-dir");
        write_package_folder(&weird, "tatolab", "camera", "2.0.0");

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Folder { path: weird },
                &AddPackageOptions::default(),
            )
            .unwrap();
        assert!(report.package_dir.ends_with("streamlib_modules/@tatolab/camera"));
    }

    #[test]
    fn re_add_replaces_cleanly_with_one_lock_entry_and_new_bytes() {
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        std::fs::write(src.path().join("only_in_v1.txt"), b"old").unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let source = AddPackageSource::Folder {
            path: src.path().to_path_buf(),
        };
        let first = app.add_package(&source, &AddPackageOptions::default()).unwrap();
        assert!(!first.replaced_existing);

        // Mutate the source: bump version, drop a file, add a file.
        std::fs::remove_file(src.path().join("only_in_v1.txt")).unwrap();
        std::fs::write(src.path().join("only_in_v2.txt"), b"new").unwrap();
        std::fs::write(
            src.path().join("streamlib.yaml"),
            manifest_yaml("tatolab", "camera", "2.1.0"),
        )
        .unwrap();

        let second = app.add_package(&source, &AddPackageOptions::default()).unwrap();
        assert!(second.replaced_existing, "re-add must report the replace");
        assert_eq!(second.version, SemVer::new(2, 1, 0));
        // New bytes in place; stale ones gone (replace, not merge).
        assert!(second.package_dir.join("only_in_v2.txt").is_file());
        assert!(!second.package_dir.join("only_in_v1.txt").exists());

        // Exactly one lock entry, at the new version, no orphan dirs.
        let lock = app.read_lockfile().unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(
            lock.packages.get("@tatolab/camera").unwrap().version,
            SemVer::new(2, 1, 0)
        );
        assert_no_partial_state(
            &app,
            &["@tatolab/camera"],
            Some(&std::fs::read(app.lockfile_path()).unwrap()),
        );
    }

    // =====================================================================
    // Hash verification
    // =====================================================================

    #[test]
    fn expected_sha256_match_passes_and_mismatch_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("camera.slpkg");
        let bytes = slpkg_bytes("tatolab", "camera", "2.0.0");
        std::fs::write(&archive, &bytes).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let source = AddPackageSource::Archive {
            path: archive.clone(),
        };

        // Match — bare hex and `sha256:`-prefixed both accepted.
        app.add_package(
            &source,
            &AddPackageOptions {
                expected_archive_sha256: Some(sha256_hex(&bytes)),
            },
        )
        .expect("matching sha must pass");
        app.add_package(
            &source,
            &AddPackageOptions {
                expected_archive_sha256: Some(format!("sha256:{}", sha256_hex(&bytes))),
            },
        )
        .expect("prefixed matching sha must pass");

        // Mismatch — typed error, and the previously-added contents + lock
        // stay byte-untouched.
        let lock_before = std::fs::read(app.lockfile_path()).unwrap();
        let err = app
            .add_package(
                &source,
                &AddPackageOptions {
                    expected_archive_sha256: Some("00".repeat(32)),
                },
            )
            .expect_err("mismatched sha must fail loud");
        assert!(matches!(err, AppModulesError::HashMismatch { .. }), "{err:?}");
        assert_no_partial_state(&app, &["@tatolab/camera"], Some(&lock_before));
    }

    // =====================================================================
    // Negative adds — each asserts NO partial state
    // =====================================================================

    #[test]
    fn add_without_manifest_is_invalid_package_with_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("junk.slpkg");
        // A valid zip that simply has no streamlib.yaml.
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::<()>::default();
            writer.start_file("readme.txt", opts).unwrap();
            writer.write_all(b"not a package").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(&archive, cursor.into_inner()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Archive { path: archive },
                &AddPackageOptions::default(),
            )
            .expect_err("manifest-less archive must fail");
        assert!(matches!(err, AppModulesError::InvalidPackage { .. }), "{err:?}");
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn add_without_package_block_is_missing_identity_with_no_residue() {
        let src = tempfile::tempdir().unwrap();
        // Project-flavor manifest: valid yaml, no `package:` block.
        std::fs::write(src.path().join("streamlib.yaml"), "dependencies: {}\n").unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .expect_err("identity-less manifest must fail");
        assert!(
            matches!(err, AppModulesError::MissingPackageIdentity { .. }),
            "{err:?}"
        );
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn add_unknown_magic_is_unsupported_archive_with_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("junk.slpkg");
        std::fs::write(&archive, b"definitely not an archive").unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Archive { path: archive },
                &AddPackageOptions::default(),
            )
            .expect_err("unknown magic must fail");
        assert!(
            matches!(err, AppModulesError::UnsupportedArchive { .. }),
            "{err:?}"
        );
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn add_zip_with_path_traversal_is_extract_failed_with_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.slpkg");
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::FileOptions::<()>::default();
            writer.start_file("../escape.txt", opts).unwrap();
            writer.write_all(b"evil").unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(&archive, cursor.into_inner()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Archive { path: archive },
                &AddPackageOptions::default(),
            )
            .expect_err("traversal zip must fail");
        assert!(matches!(err, AppModulesError::ExtractFailed { .. }), "{err:?}");
        assert_no_partial_state(&app, &[], None);
        assert!(!app_root.path().join("escape.txt").exists());
        assert!(!app.modules_dir().join("escape.txt").exists());
    }

    #[test]
    fn add_tar_with_path_traversal_is_extract_failed_with_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.tar.gz");
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let body = b"evil";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        // Forge the entry name directly — the tar writer refuses `..` paths,
        // but a hostile archive carries them anyway.
        {
            let name_bytes = b"../escape.txt";
            header.as_old_mut().name[..name_bytes.len()].copy_from_slice(name_bytes);
        }
        header.set_cksum();
        builder.append(&header, body.as_slice()).unwrap();
        std::fs::write(&archive, builder.into_inner().unwrap().finish().unwrap()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Archive { path: archive },
                &AddPackageOptions::default(),
            )
            .expect_err("traversal tar must fail");
        assert!(matches!(err, AppModulesError::ExtractFailed { .. }), "{err:?}");
        assert_no_partial_state(&app, &[], None);
        assert!(!app_root.path().join("escape.txt").exists());
    }

    #[test]
    fn add_tar_with_absolute_symlink_is_extract_failed_with_no_residue() {
        // A hostile archive whose symlink targets an absolute host path must
        // be refused, with nothing materialized — the self-contained contract.
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.tar.gz");
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let body = b"manifest";
        let mut fh = tar::Header::new_gnu();
        fh.set_size(body.len() as u64);
        fh.set_mode(0o644);
        fh.set_cksum();
        builder
            .append_data(&mut fh, "streamlib.yaml", body.as_slice())
            .unwrap();
        let mut lh = tar::Header::new_gnu();
        lh.set_entry_type(tar::EntryType::Symlink);
        lh.set_size(0);
        lh.set_mode(0o777);
        builder.append_link(&mut lh, "passwd", "/etc/passwd").unwrap();
        std::fs::write(&archive, builder.into_inner().unwrap().finish().unwrap()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Archive { path: archive },
                &AddPackageOptions::default(),
            )
            .expect_err("absolute-target symlink tar must fail");
        assert!(matches!(err, AppModulesError::ExtractFailed { .. }), "{err:?}");
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn add_tar_with_internal_relative_symlink_succeeds() {
        // A benign internal symlink is allowed; the package materializes and
        // locks normally.
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("ok.tar.gz");
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (path, body) in [
            ("streamlib.yaml", manifest_yaml("tatolab", "camera", "2.0.0")),
            ("schemas/foo_frame.yaml", SCHEMA_YAML.to_string()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        let mut lh = tar::Header::new_gnu();
        lh.set_entry_type(tar::EntryType::Symlink);
        lh.set_size(0);
        lh.set_mode(0o777);
        builder.append_link(&mut lh, "alias.yaml", "streamlib.yaml").unwrap();
        std::fs::write(&archive, builder.into_inner().unwrap().finish().unwrap()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Archive { path: archive },
                &AddPackageOptions::default(),
            )
            .expect("internal relative symlink must be allowed");
        assert_eq!(report.package, pkg_ref("tatolab", "camera"));
        assert!(report.package_dir.join("streamlib.yaml").is_file());
    }

    #[test]
    fn add_sweeps_orphan_staging_dirs_but_spares_current_process_entries() {
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let modules_dir = app.modules_dir();
        std::fs::create_dir_all(&modules_dir).unwrap();

        // A crashed prior run's orphan (a pid that isn't this process).
        let orphan = modules_dir.join(format!("{APP_MODULES_STAGING_PREFIX}1-0"));
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("junk"), b"stale").unwrap();
        let orphan_backup =
            modules_dir.join(format!("{APP_MODULES_STAGING_PREFIX}replaced-1-999"));
        std::fs::create_dir_all(&orphan_backup).unwrap();
        // Staging entries owned by THIS live process must be spared (protects
        // a concurrent same-process add/link) — both the add-flavored
        // `.staging-<pid>-*` and the link-flavored `.staging-link-<pid>-*`.
        let mine = modules_dir.join(format!(
            "{APP_MODULES_STAGING_PREFIX}{}-424242",
            std::process::id()
        ));
        std::fs::create_dir_all(&mine).unwrap();
        let mine_link = modules_dir.join(format!(
            "{APP_MODULES_STAGING_PREFIX}link-{}-424243",
            std::process::id()
        ));
        std::os::unix::fs::symlink(src.path(), &mine_link).unwrap();

        app.add_package(
            &AddPackageSource::Folder {
                path: src.path().to_path_buf(),
            },
            &AddPackageOptions::default(),
        )
        .unwrap();

        // Mentally revert the sweep and both orphans survive → this fails.
        assert!(!orphan.exists(), "orphan staging dir must be swept");
        assert!(!orphan_backup.exists(), "orphan promote backup must be swept");
        assert!(mine.exists(), "current-process add staging dir must be spared");
        // Mentally revert the `link-` prefix strip in the sweep and the pid
        // parses as "link" ⇒ None ⇒ this current-process link staging entry is
        // wrongly swept → this fails.
        assert!(
            std::fs::symlink_metadata(&mine_link).is_ok(),
            "current-process link staging entry must be spared"
        );
    }

    #[test]
    fn add_unsupported_url_scheme_is_typed_error() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Url {
                    url: "ftp://example.com/pkg.slpkg".into(),
                },
                &AddPackageOptions::default(),
            )
            .expect_err("ftp must be rejected");
        assert!(
            matches!(err, AppModulesError::UnsupportedSource { .. }),
            "{err:?}"
        );
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn add_missing_file_url_is_fetch_failed() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .add_package(
                &AddPackageSource::Url {
                    url: "file:///definitely/not/here.slpkg".into(),
                },
                &AddPackageOptions::default(),
            )
            .expect_err("missing file URL must fail");
        assert!(matches!(err, AppModulesError::FetchFailed { .. }), "{err:?}");
        assert_no_partial_state(&app, &[], None);
    }

    // =====================================================================
    // Remove
    // =====================================================================

    #[test]
    fn remove_deletes_folder_and_lock_entry_leaving_siblings_intact() {
        let src_a = tempfile::tempdir().unwrap();
        write_package_folder(src_a.path(), "tatolab", "camera", "2.0.0");
        let src_b = tempfile::tempdir().unwrap();
        write_package_folder(src_b.path(), "tatolab", "mic", "1.0.0");

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        for src in [&src_a, &src_b] {
            app.add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();
        }

        let report = app.remove_package(&pkg_ref("tatolab", "camera")).unwrap();
        assert_eq!(report.version, Some(SemVer::new(2, 0, 0)));
        assert!(report.package_dir_removed);
        assert!(report.lockfile_entry_removed);
        assert!(!report.package_dir.exists());

        // Sibling untouched, lock still records it.
        let mic_dir = app.package_dir(&pkg_ref("tatolab", "mic"));
        assert!(mic_dir.join("streamlib.yaml").is_file());
        let lock = app.read_lockfile().unwrap();
        assert!(!lock.packages.contains_key("@tatolab/camera"));
        assert!(lock.packages.contains_key("@tatolab/mic"));
    }

    #[test]
    fn remove_absent_package_is_not_installed() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .remove_package(&pkg_ref("tatolab", "never-added"))
            .expect_err("removing an absent package must fail loud");
        assert!(matches!(err, AppModulesError::NotInstalled { .. }), "{err:?}");
    }

    #[test]
    fn remove_with_lock_entry_but_missing_folder_heals_the_lock() {
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let added = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();
        // Someone rm -rf'd the folder out from under the lock.
        std::fs::remove_dir_all(&added.package_dir).unwrap();

        let report = app.remove_package(&pkg_ref("tatolab", "camera")).unwrap();
        assert!(!report.package_dir_removed);
        assert!(report.lockfile_entry_removed);
        assert!(
            !app.read_lockfile()
                .unwrap()
                .packages
                .contains_key("@tatolab/camera")
        );
    }

    // =====================================================================
    // Link / unlink
    // =====================================================================

    /// The linked slot is a symlink pointing at the canonical checkout.
    fn assert_symlink_to(slot: &Path, expected_target: &Path) {
        let meta = std::fs::symlink_metadata(slot)
            .unwrap_or_else(|e| panic!("slot {} missing: {e}", slot.display()));
        assert!(
            meta.file_type().is_symlink(),
            "slot {} must be a symlink, not a copy",
            slot.display()
        );
        assert_eq!(
            std::fs::read_link(slot).unwrap(),
            expected_target,
            "symlink target mismatch"
        );
    }

    #[test]
    fn link_symlinks_checkout_and_locks_link_source() {
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let canonical = std::fs::canonicalize(checkout.path()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app.link_package(checkout.path()).expect("link must succeed");

        assert_eq!(report.package, pkg_ref("tatolab", "camera"));
        assert_eq!(report.version, SemVer::new(2, 0, 0));
        assert!(!report.replaced_existing);
        assert_eq!(report.package_dir, app.package_dir(&report.package));
        assert_eq!(report.link_target, canonical);
        // The slot is a symlink to the checkout (NOT a copy).
        assert_symlink_to(&report.package_dir, &canonical);
        // Files resolve through the link.
        assert!(report.package_dir.join("streamlib.yaml").is_file());

        // Lock records a Link source pointing at the canonical checkout.
        let lock = app.read_lockfile().unwrap();
        let entry = lock.packages.get("@tatolab/camera").expect("locked");
        assert_eq!(entry.version, SemVer::new(2, 0, 0));
        match &entry.source {
            LockfileSource::Link { path } => assert_eq!(path, &canonical),
            other => panic!("expected Link source, got {other:?}"),
        }
        assert_no_partial_state(
            &app,
            &["@tatolab/camera"],
            Some(&std::fs::read(app.lockfile_path()).unwrap()),
        );
    }

    #[test]
    fn link_reflects_checkout_edits_live() {
        // The whole point of link: an edit in the checkout is visible through
        // the slot with no re-link, because the slot is a live symlink.
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app.link_package(checkout.path()).unwrap();

        // Edit the checkout AFTER linking.
        std::fs::write(checkout.path().join("added_after_link.txt"), b"live").unwrap();
        assert_eq!(
            std::fs::read(report.package_dir.join("added_after_link.txt")).unwrap(),
            b"live",
            "an edit in the checkout must be live through the link"
        );
    }

    #[test]
    fn link_identity_comes_from_manifest_not_dir_name() {
        // A checkout named `weird-dir` declaring @tatolab/camera lands at
        // streamlib_modules/@tatolab/camera.
        let parent = tempfile::tempdir().unwrap();
        let weird = parent.path().join("weird-dir");
        write_package_folder(&weird, "tatolab", "camera", "2.0.0");

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let report = app.link_package(&weird).unwrap();
        assert!(report.package_dir.ends_with("streamlib_modules/@tatolab/camera"));
        assert_symlink_to(&report.package_dir, &std::fs::canonicalize(&weird).unwrap());
    }

    #[test]
    fn relink_replaces_cleanly_with_one_lock_entry() {
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let canonical = std::fs::canonicalize(checkout.path()).unwrap();
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());

        let first = app.link_package(checkout.path()).unwrap();
        assert!(!first.replaced_existing);
        let second = app.link_package(checkout.path()).unwrap();
        assert!(second.replaced_existing, "relink must report the replace");
        assert_symlink_to(&second.package_dir, &canonical);

        // Exactly one lock entry, still a Link, no orphan/staging residue.
        let lock = app.read_lockfile().unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert!(matches!(
            lock.packages.get("@tatolab/camera").unwrap().source,
            LockfileSource::Link { .. }
        ));
        assert_no_partial_state(
            &app,
            &["@tatolab/camera"],
            Some(&std::fs::read(app.lockfile_path()).unwrap()),
        );
    }

    #[test]
    fn relink_over_dangling_symlink_reports_replaced() {
        // Relinking over a slot whose prior link dangles (checkout deleted)
        // must report the replace and land a fresh symlink. Mentally revert
        // the `symlink_metadata` displaced-detection in
        // `promote_staged_package_root` to `exists()` and the dangling slot
        // reads as absent → replaced_existing would be false.
        let first_checkout = tempfile::tempdir().unwrap();
        write_package_folder(first_checkout.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let first = app.link_package(first_checkout.path()).unwrap();
        // Delete the first checkout: the slot symlink now dangles.
        std::fs::remove_dir_all(first_checkout.path()).unwrap();
        assert!(
            std::fs::symlink_metadata(&first.package_dir)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // Relink from a fresh checkout of the same package.
        let second_checkout = tempfile::tempdir().unwrap();
        write_package_folder(second_checkout.path(), "tatolab", "camera", "2.0.0");
        let canonical = std::fs::canonicalize(second_checkout.path()).unwrap();
        let second = app.link_package(second_checkout.path()).unwrap();
        assert!(
            second.replaced_existing,
            "relink over a dangling symlink must report the replace"
        );
        assert_symlink_to(&second.package_dir, &canonical);
        assert_no_partial_state(
            &app,
            &["@tatolab/camera"],
            Some(&std::fs::read(app.lockfile_path()).unwrap()),
        );
    }

    #[test]
    fn link_over_existing_added_copy_replaces_with_symlink() {
        // Precedence: linking a package whose slot already holds an ADDED
        // (copied) package replaces the copy with a symlink — last write wins,
        // consistent with re-add. The lock flips Path -> Link.
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let canonical = std::fs::canonicalize(checkout.path()).unwrap();
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());

        // First an add (a real copied dir).
        let added = app
            .add_package(
                &AddPackageSource::Folder {
                    path: checkout.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();
        assert!(
            !added
                .package_dir
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "add must materialize a real copy"
        );
        assert!(matches!(
            app.read_lockfile().unwrap().packages.get("@tatolab/camera").unwrap().source,
            LockfileSource::Path { .. }
        ));

        // Now link over it.
        let linked = app.link_package(checkout.path()).unwrap();
        assert!(linked.replaced_existing, "link over add must report replace");
        assert_symlink_to(&linked.package_dir, &canonical);
        let lock = app.read_lockfile().unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert!(matches!(
            lock.packages.get("@tatolab/camera").unwrap().source,
            LockfileSource::Link { .. }
        ));
        assert_no_partial_state(
            &app,
            &["@tatolab/camera"],
            Some(&std::fs::read(app.lockfile_path()).unwrap()),
        );
    }

    #[test]
    fn link_non_package_dir_is_typed_error_with_no_residue() {
        // A directory with no streamlib.yaml is not a package.
        let not_a_pkg = tempfile::tempdir().unwrap();
        std::fs::write(not_a_pkg.path().join("readme.txt"), b"hi").unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .link_package(not_a_pkg.path())
            .expect_err("linking a non-package dir must fail");
        assert!(matches!(err, AppModulesError::InvalidPackage { .. }), "{err:?}");
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn link_manifest_without_identity_is_missing_identity_with_no_residue() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("streamlib.yaml"), "dependencies: {}\n").unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .link_package(src.path())
            .expect_err("identity-less manifest must fail");
        assert!(
            matches!(err, AppModulesError::MissingPackageIdentity { .. }),
            "{err:?}"
        );
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn link_non_directory_path_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("camera.slpkg");
        std::fs::write(&file, b"archive-not-a-folder").unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .link_package(&file)
            .expect_err("linking a file must fail");
        assert!(
            matches!(err, AppModulesError::LinkPathNotADirectory { .. }),
            "{err:?}"
        );
        assert_no_partial_state(&app, &[], None);
    }

    #[test]
    fn unlink_removes_symlink_and_lock_entry_leaving_checkout_intact() {
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let linked = app.link_package(checkout.path()).unwrap();

        let report = app.unlink_package(&pkg_ref("tatolab", "camera")).unwrap();
        assert!(report.link_removed);
        assert!(report.lockfile_entry_removed);
        assert_eq!(
            report.link_target.as_deref(),
            Some(std::fs::canonicalize(checkout.path()).unwrap().as_path())
        );
        // Symlink gone, lock entry gone.
        assert!(std::fs::symlink_metadata(&linked.package_dir).is_err());
        assert!(
            !app.read_lockfile()
                .unwrap()
                .packages
                .contains_key("@tatolab/camera")
        );
        // The linked checkout on disk is untouched.
        assert!(checkout.path().join("streamlib.yaml").is_file());
        assert_no_partial_state(&app, &[], Some(&std::fs::read(app.lockfile_path()).unwrap()));
    }

    #[test]
    fn unlink_non_linked_added_copy_is_typed_error_and_copy_survives() {
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let added = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();

        let err = app
            .unlink_package(&pkg_ref("tatolab", "camera"))
            .expect_err("unlinking an added copy must fail loud");
        match err {
            AppModulesError::NotLinked {
                present_as_added_copy,
                ..
            } => assert!(present_as_added_copy, "must flag the added-copy case"),
            other => panic!("expected NotLinked, got {other:?}"),
        }
        // The added copy and its lock entry are untouched.
        assert!(added.package_dir.join("streamlib.yaml").is_file());
        assert!(
            app.read_lockfile()
                .unwrap()
                .packages
                .contains_key("@tatolab/camera")
        );
    }

    #[test]
    fn unlink_absent_package_is_not_linked_error() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .unlink_package(&pkg_ref("tatolab", "never-linked"))
            .expect_err("unlinking an absent package must fail loud");
        match err {
            AppModulesError::NotLinked {
                present_as_added_copy,
                ..
            } => assert!(!present_as_added_copy),
            other => panic!("expected NotLinked, got {other:?}"),
        }
    }

    #[test]
    fn unlink_dangling_link_heals_symlink_and_entry() {
        // Link, then delete the checkout out from under the symlink. Unlink
        // still removes the dangling symlink and its lock entry.
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let linked = app.link_package(checkout.path()).unwrap();

        // The symlink now dangles.
        std::fs::remove_dir_all(checkout.path()).unwrap();
        assert!(
            std::fs::symlink_metadata(&linked.package_dir)
                .unwrap()
                .file_type()
                .is_symlink(),
            "slot is still a (now dangling) symlink"
        );

        let report = app.unlink_package(&pkg_ref("tatolab", "camera")).unwrap();
        assert!(report.link_removed);
        assert!(report.lockfile_entry_removed);
        assert!(std::fs::symlink_metadata(&linked.package_dir).is_err());
    }

    #[test]
    fn remove_on_linked_slot_unlinks_without_deleting_checkout() {
        // `remove` works on a linked slot too — it unlinks the symlink via the
        // shared entry-removal helper and never follows into the checkout.
        // Mentally revert `remove_dir_entry_all` to `remove_dir_all` and this
        // either errors on the symlink or deletes the checkout contents.
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let linked = app.link_package(checkout.path()).unwrap();

        let report = app.remove_package(&pkg_ref("tatolab", "camera")).unwrap();
        assert!(report.package_dir_removed);
        assert!(report.lockfile_entry_removed);
        assert!(std::fs::symlink_metadata(&linked.package_dir).is_err());
        // Critically: the linked checkout's contents survive.
        assert!(
            checkout.path().join("streamlib.yaml").is_file(),
            "remove must unlink the symlink, never delete the checkout"
        );
        assert!(checkout.path().join("schemas/foo_frame.yaml").is_file());
    }

    // =====================================================================
    // Lockfile plumbing
    // =====================================================================

    #[test]
    fn read_lockfile_absent_is_empty_and_corrupt_is_typed_error() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let lock = app.read_lockfile().unwrap();
        assert!(lock.packages.is_empty());

        std::fs::write(app.lockfile_path(), "{ not yaml at all").unwrap();
        let err = app.read_lockfile().expect_err("corrupt lock must fail loud");
        assert!(
            matches!(err, AppModulesError::LockfileReadFailed { .. }),
            "{err:?}"
        );
    }

    // =====================================================================
    // Install from lockfile — reproduce streamlib_modules/ from streamlib.lock
    // =====================================================================

    /// Hand-write a `streamlib.lock` at an app root from raw entries.
    fn write_modules_lock(app: &AppModulesDir, entries: &[(&str, LockfileEntry)]) {
        let mut lock = Lockfile {
            version: 1,
            packages: Default::default(),
        };
        for (key, entry) in entries {
            lock.packages.insert(key.to_string(), entry.clone());
        }
        write_modules_lockfile(&app.lockfile_path(), &lock).unwrap();
    }

    /// The flagship: a clean checkout carrying ONLY `streamlib.lock` — with one
    /// entry of each reproducible source kind (path / archive / url / link) —
    /// reproduces a byte-equivalent, hash-verified `streamlib_modules/`.
    #[test]
    fn install_reproduces_each_source_kind_from_a_clean_checkout() {
        // --- Build the four sources on disk -----------------------------
        let path_src = tempfile::tempdir().unwrap();
        write_package_folder(path_src.path(), "tatolab", "via-path", "1.0.0");

        let archives = tempfile::tempdir().unwrap();
        let archive_file = archives.path().join("via-archive.slpkg");
        std::fs::write(
            &archive_file,
            slpkg_bytes("tatolab", "via-archive", "1.0.0"),
        )
        .unwrap();

        let url_archive_file = archives.path().join("via-url.slpkg");
        std::fs::write(
            &url_archive_file,
            slpkg_bytes("tatolab", "via-url", "1.0.0"),
        )
        .unwrap();
        let url = format!("file://{}", url_archive_file.display());

        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "via-link", "1.0.0");
        let checkout_canonical = std::fs::canonicalize(checkout.path()).unwrap();

        // --- Record the decision in a SOURCE app's streamlib.lock -------
        let source_app_root = tempfile::tempdir().unwrap();
        let source_app = AppModulesDir::at(source_app_root.path());
        source_app
            .add_package(
                &AddPackageSource::Folder {
                    path: path_src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();
        source_app
            .add_package(
                &AddPackageSource::Archive {
                    path: archive_file.clone(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();
        source_app
            .add_package(
                &AddPackageSource::Url { url: url.clone() },
                &AddPackageOptions::default(),
            )
            .unwrap();
        source_app.link_package(checkout.path()).unwrap();

        // --- Clean checkout: ONLY the lockfile is present ---------------
        let dest_root = tempfile::tempdir().unwrap();
        let dest = AppModulesDir::at(dest_root.path());
        std::fs::copy(source_app.lockfile_path(), dest.lockfile_path()).unwrap();
        assert!(!dest.modules_dir().exists(), "precondition: no modules dir");
        let lock_bytes_before = std::fs::read(dest.lockfile_path()).unwrap();

        // --- Reproduce --------------------------------------------------
        let report = dest.install_from_lockfile().expect("install must succeed");
        assert_eq!(report.packages.len(), 4);

        // Every materialized slot is a real dir with a manifest whose re-hash
        // matches the lockfile pin; the link slot is a symlink to the checkout.
        for name in ["via-path", "via-archive", "via-url"] {
            let slot = dest.package_dir(&pkg_ref("tatolab", name));
            assert!(
                slot.join("streamlib.yaml").is_file(),
                "{name} slot missing manifest"
            );
            assert!(
                !std::fs::symlink_metadata(&slot).unwrap().file_type().is_symlink(),
                "{name} must be a real copy, not a symlink"
            );
            let locked = source_app
                .read_lockfile()
                .unwrap()
                .packages
                .get(&format!("@tatolab/{name}"))
                .unwrap()
                .content_hash
                .clone();
            assert_eq!(
                content_hash_for_package_dir(&slot).unwrap(),
                locked,
                "{name} reproduced content hash must match the lock pin"
            );
        }
        let link_slot = dest.package_dir(&pkg_ref("tatolab", "via-link"));
        let link_meta = std::fs::symlink_metadata(&link_slot).unwrap();
        assert!(link_meta.file_type().is_symlink(), "link slot must be a symlink");
        assert_eq!(std::fs::read_link(&link_slot).unwrap(), checkout_canonical);

        // Report classifies each kind correctly.
        let kind_of = |name: &str| {
            report
                .packages
                .iter()
                .find(|p| p.package == pkg_ref("tatolab", name))
                .unwrap()
                .kind
        };
        assert_eq!(kind_of("via-path"), InstalledFromLockKind::Materialized);
        assert_eq!(kind_of("via-archive"), InstalledFromLockKind::Materialized);
        assert_eq!(kind_of("via-url"), InstalledFromLockKind::Materialized);
        assert_eq!(kind_of("via-link"), InstalledFromLockKind::Linked);

        // Install NEVER rewrites the lockfile it reproduced from.
        assert_eq!(
            std::fs::read(dest.lockfile_path()).unwrap(),
            lock_bytes_before,
            "install must not modify streamlib.lock"
        );
    }

    /// Running install twice yields the same folder (idempotent).
    #[test]
    fn install_is_idempotent() {
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        let source_app_root = tempfile::tempdir().unwrap();
        let source_app = AppModulesDir::at(source_app_root.path());
        source_app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &AddPackageOptions::default(),
            )
            .unwrap();

        let dest_root = tempfile::tempdir().unwrap();
        let dest = AppModulesDir::at(dest_root.path());
        std::fs::copy(source_app.lockfile_path(), dest.lockfile_path()).unwrap();

        let first = dest.install_from_lockfile().unwrap();
        assert_eq!(first.packages.len(), 1);
        assert!(!first.packages[0].replaced_existing, "first install is fresh");
        let slot = dest.package_dir(&pkg_ref("tatolab", "camera"));
        let manifest_after_first = std::fs::read(slot.join("streamlib.yaml")).unwrap();
        let hash_after_first = content_hash_for_package_dir(&slot).unwrap();

        let second = dest.install_from_lockfile().unwrap();
        assert!(
            second.packages[0].replaced_existing,
            "second install replaces the existing slot"
        );
        assert_eq!(
            std::fs::read(slot.join("streamlib.yaml")).unwrap(),
            manifest_after_first,
            "re-install must be byte-identical"
        );
        assert_eq!(content_hash_for_package_dir(&slot).unwrap(), hash_after_first);
        // No staging residue after two runs.
        assert_no_partial_state(
            &dest,
            &["@tatolab/camera"],
            Some(&std::fs::read(dest.lockfile_path()).unwrap()),
        );
    }

    #[test]
    fn install_missing_lockfile_is_typed_error() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        let err = app
            .install_from_lockfile()
            .expect_err("install with no lockfile must fail loud");
        assert!(
            matches!(err, AppModulesError::InstallLockfileMissing { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn install_empty_lockfile_is_noop_success() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        write_modules_lock(&app, &[]);
        let report = app.install_from_lockfile().expect("empty lock installs");
        assert!(report.packages.is_empty());
    }

    #[test]
    fn install_content_hash_mismatch_is_typed_error_with_no_partial_state() {
        // A path source that is valid but whose recorded content hash is wrong
        // (a tampered/changed source) is refused BEFORE any slot is promoted.
        let src = tempfile::tempdir().unwrap();
        write_package_folder(src.path(), "tatolab", "camera", "2.0.0");
        let canonical = std::fs::canonicalize(src.path()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        write_modules_lock(
            &app,
            &[(
                "@tatolab/camera",
                LockfileEntry {
                    version: SemVer::new(2, 0, 0),
                    source: LockfileSource::Path { path: canonical },
                    content_hash: "sha256:deadbeef".to_string(),
                },
            )],
        );
        let lock_before = std::fs::read(app.lockfile_path()).unwrap();

        let err = app
            .install_from_lockfile()
            .expect_err("content hash mismatch must fail");
        match err {
            AppModulesError::InstallContentHashMismatch { package, .. } => {
                assert_eq!(package, pkg_ref("tatolab", "camera"));
            }
            other => panic!("expected InstallContentHashMismatch, got {other:?}"),
        }
        // No slot promoted; no staging residue; lock untouched.
        assert_no_partial_state(&app, &[], Some(&lock_before));
    }

    #[test]
    fn install_missing_path_source_is_typed_error_naming_package() {
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        write_modules_lock(
            &app,
            &[(
                "@tatolab/camera",
                LockfileEntry {
                    version: SemVer::new(2, 0, 0),
                    source: LockfileSource::Path {
                        path: "/definitely/not/here".into(),
                    },
                    content_hash: "sha256:abc".to_string(),
                },
            )],
        );
        let err = app
            .install_from_lockfile()
            .expect_err("gone path source must fail");
        match err {
            AppModulesError::InstallSourceUnavailable { package, .. } => {
                assert_eq!(package, pkg_ref("tatolab", "camera"));
            }
            other => panic!("expected InstallSourceUnavailable, got {other:?}"),
        }
        assert_no_partial_state(&app, &[], Some(&std::fs::read(app.lockfile_path()).unwrap()));
    }

    #[test]
    fn install_missing_archive_and_url_sources_are_typed_errors() {
        // A gone archive file.
        let app_a_root = tempfile::tempdir().unwrap();
        let app_a = AppModulesDir::at(app_a_root.path());
        write_modules_lock(
            &app_a,
            &[(
                "@tatolab/camera",
                LockfileEntry {
                    version: SemVer::new(2, 0, 0),
                    source: LockfileSource::Archive {
                        path: "/definitely/not/here.slpkg".into(),
                        archive_sha256: "ab".repeat(32),
                    },
                    content_hash: "sha256:abc".to_string(),
                },
            )],
        );
        assert!(matches!(
            app_a.install_from_lockfile().expect_err("gone archive must fail"),
            AppModulesError::InstallSourceUnavailable { .. }
        ));

        // An unreachable file:// URL (offline).
        let app_b_root = tempfile::tempdir().unwrap();
        let app_b = AppModulesDir::at(app_b_root.path());
        write_modules_lock(
            &app_b,
            &[(
                "@tatolab/camera",
                LockfileEntry {
                    version: SemVer::new(2, 0, 0),
                    source: LockfileSource::Url {
                        url: "file:///definitely/not/here.slpkg".to_string(),
                        archive_sha256: "ab".repeat(32),
                    },
                    content_hash: "sha256:abc".to_string(),
                },
            )],
        );
        assert!(matches!(
            app_b.install_from_lockfile().expect_err("gone url must fail"),
            AppModulesError::InstallSourceUnavailable { .. }
        ));
    }

    #[test]
    fn install_archive_sha_mismatch_is_typed_error() {
        // A valid archive whose bytes don't match the recorded archive_sha256.
        let archives = tempfile::tempdir().unwrap();
        let archive_file = archives.path().join("camera.slpkg");
        std::fs::write(&archive_file, slpkg_bytes("tatolab", "camera", "2.0.0")).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        write_modules_lock(
            &app,
            &[(
                "@tatolab/camera",
                LockfileEntry {
                    version: SemVer::new(2, 0, 0),
                    source: LockfileSource::Archive {
                        path: archive_file,
                        archive_sha256: "00".repeat(32),
                    },
                    content_hash: "sha256:abc".to_string(),
                },
            )],
        );
        match app
            .install_from_lockfile()
            .expect_err("archive sha mismatch must fail")
        {
            AppModulesError::InstallArchiveHashMismatch { package, .. } => {
                assert_eq!(package, pkg_ref("tatolab", "camera"));
            }
            other => panic!("expected InstallArchiveHashMismatch, got {other:?}"),
        }
        assert_no_partial_state(&app, &[], Some(&std::fs::read(app.lockfile_path()).unwrap()));
    }

    #[test]
    fn install_dangling_link_target_is_typed_error_naming_package() {
        // A link entry whose checkout target no longer exists — a dev-only link
        // isn't reproducible on another machine; that's an explicit error.
        let checkout = tempfile::tempdir().unwrap();
        write_package_folder(checkout.path(), "tatolab", "camera", "2.0.0");
        let canonical = std::fs::canonicalize(checkout.path()).unwrap();
        std::fs::remove_dir_all(checkout.path()).unwrap();

        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        write_modules_lock(
            &app,
            &[(
                "@tatolab/camera",
                LockfileEntry {
                    version: SemVer::new(2, 0, 0),
                    source: LockfileSource::Link { path: canonical },
                    content_hash: "sha256:abc".to_string(),
                },
            )],
        );
        match app
            .install_from_lockfile()
            .expect_err("dangling link target must fail")
        {
            AppModulesError::InstallDanglingLinkTarget { package, .. } => {
                assert_eq!(package, pkg_ref("tatolab", "camera"));
            }
            other => panic!("expected InstallDanglingLinkTarget, got {other:?}"),
        }
        // No slot created for the un-reproducible link.
        let slot = app.package_dir(&pkg_ref("tatolab", "camera"));
        assert!(std::fs::symlink_metadata(&slot).is_err());
    }

    #[test]
    fn install_unsupported_source_kind_is_typed_error() {
        // A registry/git entry can't be reproduced by install (add/link never
        // writes these into streamlib.lock, but be defensive).
        let app_root = tempfile::tempdir().unwrap();
        let app = AppModulesDir::at(app_root.path());
        write_modules_lock(
            &app,
            &[(
                "@tatolab/camera",
                LockfileEntry {
                    version: SemVer::new(2, 0, 0),
                    source: LockfileSource::Registry {
                        url: "https://packages.streamlib.dev".to_string(),
                    },
                    content_hash: "sha256:abc".to_string(),
                },
            )],
        );
        match app
            .install_from_lockfile()
            .expect_err("registry source must be refused")
        {
            AppModulesError::InstallUnsupportedSource { package, kind } => {
                assert_eq!(package, pkg_ref("tatolab", "camera"));
                assert_eq!(kind, "registry");
            }
            other => panic!("expected InstallUnsupportedSource, got {other:?}"),
        }
    }
}
