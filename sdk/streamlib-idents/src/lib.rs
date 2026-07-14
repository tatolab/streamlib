// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Structured schema identifiers, semver, manifest/lockfile types, and the
//! `streamlib.yaml` dependency resolver.
//!
//! The structured-everywhere rule: identifiers are constructed by codegen or
//! by typed YAML/JSON deserialization. There is no public `parse` API and
//! none should be added — see `docs/architecture/schema-identity-and-packaging.md`.

pub mod app_modules;
pub mod archive;
mod catalog;
mod error;
mod git;
mod ident;
pub mod link_marker;
mod lockfile;
mod manifest;
mod registry;
mod release;
mod resolver;
mod semver;

pub use catalog::{
    CATALOG_INDEX_PATH, CatalogClient, CatalogConfig, CatalogIndexLine, CatalogPort,
    CatalogProcessor, CatalogRuntime, CatalogSchemaRef, PackageCatalog, package_catalog_file_name,
    parse_catalog_index_ndjson, render_catalog_index_ndjson, schema_jtd_file_name,
};

pub use error::{IdentError, IdentResult, ResolverError, ResolverResult};
pub use git::fetch_git;
pub use ident::{
    ModuleIdent, Org, Package, PackageRef, SchemaIdent, TypeName, validate_org, validate_package,
    validate_type,
};
pub use lockfile::{
    APP_LOCKFILE_NAME, CODEGEN_LOCKFILE_NAME, Lockfile, LockfileEntry, LockfileSource,
    MODULES_LOCKFILE_NAME, compute_content_hash, hash_content, read_lockfile, write_app_lockfile,
    write_lockfile, write_modules_lockfile,
};
pub use manifest::{
    DependencySpec, GitDependency, Manifest, PackageMetadata, PathDependency, RegistryDependency,
    SchemaEntry,
};
pub use registry::{
    DEFAULT_REGISTRY_URL, LINK_CHECKOUT_ENV, REGISTRY_URL_ENV, RELEASE_MANIFEST_CHANNEL,
    RELEASE_MANIFEST_FILE, RegistryClient, RegistryConfig, select_version,
};
pub use release::{
    RELEASE_MANIFEST_FORMAT, ReleaseManifest, ReleaseManifestMember, crates_missing_from_release,
};
pub use resolver::{
    ResolvedPackage, ResolvedPackages, ResolvedSource, ResolverOptions,
    content_hash_for_package_dir, resolve, resolve_bare_schema_name, resolve_with,
};
pub use semver::{Prerelease, PrereleaseKind, SemVer, SemVerRange};
