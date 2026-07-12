// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Structured schema identifiers, semver, manifest/lockfile types, and the
//! `streamlib.yaml` dependency resolver.
//!
//! The structured-everywhere rule: identifiers are constructed by codegen or
//! by typed YAML/JSON deserialization. There is no public `parse` API and
//! none should be added — see `docs/architecture/schema-identity-and-packaging.md`.

mod catalog;
mod error;
mod git;
mod ident;
mod lockfile;
mod manifest;
mod registry;
mod release;
mod resolver;
mod semver;

pub use catalog::{
    package_catalog_file_name, parse_catalog_index_ndjson, render_catalog_index_ndjson,
    schema_jtd_file_name, CatalogClient, CatalogConfig, CatalogIndexLine, CatalogPort,
    CatalogProcessor, CatalogRuntime, CatalogSchemaRef, PackageCatalog, CATALOG_INDEX_PATH,
};

pub use error::{IdentError, IdentResult, ResolverError, ResolverResult};
pub use git::fetch_git;
pub use ident::{
    validate_org, validate_package, validate_type, ModuleIdent, Org, Package, PackageRef,
    SchemaIdent, TypeName,
};
pub use lockfile::{
    compute_content_hash, hash_content, read_lockfile, write_app_lockfile, write_lockfile,
    Lockfile, LockfileEntry, LockfileSource, APP_LOCKFILE_NAME, LOCKFILE_NAME,
};
pub use manifest::{
    DependencySpec, GitDependency, Manifest, PackageMetadata, PathDependency, RegistryDependency,
    SchemaEntry,
};
pub use registry::{
    select_version, RegistryClient, RegistryConfig, RELEASE_MANIFEST_CHANNEL,
    RELEASE_MANIFEST_FILE, REGISTRY_TOKEN_ENV, REGISTRY_URL_ENV,
};
pub use release::{
    crates_missing_from_release, ReleaseManifest, ReleaseManifestMember, RELEASE_MANIFEST_FORMAT,
};
pub use resolver::{
    content_hash_for_package_dir, resolve, resolve_bare_schema_name, resolve_with,
    ResolvedPackage, ResolvedPackages, ResolvedSource, ResolverOptions,
};
pub use semver::{Prerelease, PrereleaseKind, SemVer, SemVerRange};
