// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Structured schema identifiers, semver, and manifest/lockfile types.
//!
//! The structured-everywhere rule: identifiers are constructed by codegen or
//! by typed YAML/JSON deserialization. There is no public `parse` API and
//! none should be added — see `docs/architecture/schema-identity-and-packaging.md`.

mod error;
mod ident;
mod lockfile;
mod manifest;
mod semver;

pub use error::{IdentError, IdentResult};
pub use ident::{validate_org, validate_package, validate_type, Org, Package, SchemaIdent, TypeName};
pub use lockfile::{Lockfile, LockfileEntry, LockfileSource};
pub use manifest::{
    DependencySpec, GitDependency, PackageManifest, PathDependency, ProjectManifest,
    RegistryDependency,
};
pub use semver::{SemVer, SemVerRange};
