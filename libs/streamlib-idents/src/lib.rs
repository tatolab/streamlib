// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Structured schema identifiers, semver, manifest/lockfile types, and the
//! `streamlib.yaml` dependency resolver.
//!
//! The structured-everywhere rule: identifiers are constructed by codegen or
//! by typed YAML/JSON deserialization. There is no public `parse` API and
//! none should be added — see `docs/architecture/schema-identity-and-packaging.md`.

mod error;
mod git;
mod ident;
mod lockfile;
mod manifest;
mod resolver;
mod semver;

pub use error::{IdentError, IdentResult, ResolverError, ResolverResult};
pub use git::fetch_git;
pub use ident::{
    validate_org, validate_package, validate_type, Org, Package, PackageRef, SchemaIdent, TypeName,
};
pub use lockfile::{
    compute_content_hash, hash_content, read_lockfile, write_lockfile, Lockfile, LockfileEntry,
    LockfileSource, LOCKFILE_NAME,
};
pub use manifest::{
    DependencySpec, GitDependency, Manifest, PackageMetadata, PathDependency, RegistryDependency,
    SchemaEntry,
};
pub use resolver::{
    resolve, resolve_bare_schema_name, resolve_with, ResolvedPackage, ResolvedPackages,
    ResolvedSource, ResolverOptions,
};
pub use semver::{SemVer, SemVerRange};
