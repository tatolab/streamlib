// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;
use thiserror::Error;

pub type IdentResult<T> = Result<T, IdentError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentError {
    #[error("org segment is empty")]
    EmptyOrg,

    #[error("package segment is empty")]
    EmptyPackage,

    #[error("type segment is empty")]
    EmptyType,

    #[error("org `{0}` contains invalid character `{1}` (allowed: a-z, 0-9, hyphen, must start with a-z)")]
    InvalidOrgCharacter(String, char),

    #[error("package `{0}` contains invalid character `{1}` (allowed: a-z, 0-9, hyphen, must start with a-z)")]
    InvalidPackageCharacter(String, char),

    #[error("type `{0}` contains invalid character `{1}` (allowed: a-z, A-Z, 0-9, must start with A-Z)")]
    InvalidTypeCharacter(String, char),

    #[error("org `{0}` must start with a-z")]
    OrgMustStartWithLowercase(String),

    #[error("package `{0}` must start with a-z")]
    PackageMustStartWithLowercase(String),

    #[error("type `{0}` must start with A-Z (PascalCase)")]
    TypeMustStartWithUppercase(String),

    #[error("invalid semver `{0}`: {1}")]
    InvalidSemVer(String, String),

    #[error("invalid semver range `{0}`: {1}")]
    InvalidSemVerRange(String, String),
}

pub type ResolverResult<T> = Result<T, ResolverError>;

/// Errors raised by the dependency resolver and lockfile writer.
#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("failed to read manifest at `{path}`: {source}")]
    ManifestRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse manifest at `{path}`: {source}")]
    ManifestParse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("manifest `{path}` declares dependency `{dep_key}` but it must match the form `@org/name` (was `{actual}`)")]
    InvalidDependencyKey {
        path: PathBuf,
        dep_key: String,
        actual: String,
    },

    #[error("manifest `{path}` declares package id `{declared}` which doesn't match the dependency edge `{requested}`")]
    PackageIdMismatch {
        path: PathBuf,
        declared: String,
        requested: String,
    },

    #[error("dependency `{name}` declared at `{from}` resolves to version `{found}` which doesn't satisfy range `{range}`")]
    VersionRangeUnsatisfied {
        name: String,
        from: PathBuf,
        found: String,
        range: String,
    },

    #[error("dependency `{name}` resolution conflict: range `{range_a}` (from `{from_a}`) and `{range_b}` (from `{from_b}`) have no overlap")]
    VersionRangeConflict {
        name: String,
        range_a: String,
        from_a: PathBuf,
        range_b: String,
        from_b: PathBuf,
    },

    #[error("circular dependency detected: {chain}")]
    CircularDependency { chain: String },

    #[error("path dependency `{name}` at `{path}` not found")]
    PathDependencyNotFound { name: String, path: PathBuf },

    #[error("path dependency `{name}` at `{path}` is not a directory")]
    PathDependencyNotDirectory { name: String, path: PathBuf },

    #[error("git dependency `{name}` ({url}) failed: {message}")]
    GitDependencyFailed {
        name: String,
        url: String,
        message: String,
    },

    #[error("`.slpkg` archive `{path}` failed to extract: {message}")]
    SlpkgExtractFailed { path: PathBuf, message: String },

    #[error("registry dependency `{name}` requires a registry, but the v1 resolver does not yet ship one")]
    RegistryNotImplemented { name: String },

    #[error("workspace `[patch]` entry for `{name}` at `{}` is a registry/git override, but the resolver only supports path overrides today (declare a `path:` patch entry pointing at a local directory)", workspace_root.display())]
    WorkspacePatchUnsupportedShape {
        name: String,
        workspace_root: PathBuf,
    },

    #[error("io error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("schema file `{path}` not found (declared by manifest `{from}`)")]
    SchemaNotFound { path: PathBuf, from: PathBuf },

    #[error(transparent)]
    Ident(#[from] IdentError),
}
