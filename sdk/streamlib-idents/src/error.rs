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

    #[error(
        "org `{0}` contains invalid character `{1}` (allowed: a-z, 0-9, hyphen, must start with a-z)"
    )]
    InvalidOrgCharacter(String, char),

    #[error(
        "package `{0}` contains invalid character `{1}` (allowed: a-z, 0-9, hyphen, must start with a-z)"
    )]
    InvalidPackageCharacter(String, char),

    #[error(
        "type `{0}` contains invalid character `{1}` (allowed: a-z, A-Z, 0-9, must start with A-Z)"
    )]
    InvalidTypeCharacter(String, char),

    #[error("org `{0}` must start with a-z")]
    OrgMustStartWithLowercase(String),

    #[error("package `{0}` must start with a-z")]
    PackageMustStartWithLowercase(String),

    #[error("type `{0}` must start with A-Z (PascalCase)")]
    TypeMustStartWithUppercase(String),

    #[error("channel name is empty")]
    EmptyChannelName,

    #[error(
        "channel `{0}` contains invalid character `{1}` (allowed: a-z, 0-9, hyphen, must start with a-z)"
    )]
    InvalidChannelNameCharacter(String, char),

    #[error("channel `{0}` must start with a-z")]
    ChannelNameMustStartWithLowercase(String),

    #[error("channel `{name}` is {len} bytes, exceeding the {max}-byte wire capacity")]
    ChannelNameTooLong {
        name: String,
        len: usize,
        max: usize,
    },

    #[error("invalid semver `{0}`: {1}")]
    InvalidSemVer(String, String),

    #[error("invalid semver range `{0}`: {1}")]
    InvalidSemVerRange(String, String),

    #[error("invalid module identifier `{0}`: {1}")]
    InvalidModuleIdent(String, String),
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

    #[error(
        "manifest `{path}` declares dependency `{dep_key}` but it must match the form `@org/name` (was `{actual}`)"
    )]
    InvalidDependencyKey {
        path: PathBuf,
        dep_key: String,
        actual: String,
    },

    #[error(
        "manifest `{path}` declares package id `{declared}` which doesn't match the dependency edge `{requested}`"
    )]
    PackageIdMismatch {
        path: PathBuf,
        declared: String,
        requested: String,
    },

    #[error(
        "dependency `{name}` declared at `{from}` resolves to version `{found}` which doesn't satisfy range `{range}`"
    )]
    VersionRangeUnsatisfied {
        name: String,
        from: PathBuf,
        found: String,
        range: String,
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

    #[error(
        "version dependency `{name}` cannot be resolved: no package source is configured. \
         Run `streamlib link` to resolve it from a local checkout, or set `{env}` to a \
         package source root (`file://<root>` or `http(s)://…`, e.g. \
         file:///path/to/slpkg-tree). A `patch:` override also resolves it from a local path."
    )]
    PackageSourceNotConfigured { name: String, env: String },

    #[error("version dependency `{name}` fetch from the package source failed: {detail}")]
    PackageSourceFetchFailed { name: String, detail: String },

    #[error(
        "version dependency `{name}` has no version satisfying range `{range}` \
         in the package source (available: {available})"
    )]
    PackageSourceNoMatchingVersion {
        name: String,
        range: String,
        available: String,
    },

    #[error("io error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("schema file `{path}` not found (declared by manifest `{from}`)")]
    SchemaNotFound { path: PathBuf, from: PathBuf },

    #[error(
        "bare schema name `{name}` is not declared in the `schemas:` map of package `{package}` \
         (resolution chain: {chain:?}). Add an entry like `{name}: {{ file: schemas/<file>.yaml }}` \
         (local) or `{name}: {{ package: \"@org/name\" }}` (imported from a dependency)."
    )]
    BareSchemaNameUnresolved {
        name: String,
        package: String,
        chain: Vec<String>,
    },

    #[error(
        "schemas: entry `{name}` in package `{package}` declares `package: {dep}` but that \
         dependency is not declared in `dependencies:` (or it failed to resolve). Add it to \
         `dependencies:` first."
    )]
    BareSchemaNameDepMissing {
        name: String,
        package: String,
        dep: String,
    },

    #[error(
        "bare schema name `{name}` resolution cycles through external imports \
         (resolution chain: {chain:?}). Two packages re-export the type from each \
         other; declare it as `{{ file: ... }}` in the package that actually owns it."
    )]
    BareSchemaNameCycle {
        name: String,
        package: String,
        chain: Vec<String>,
    },

    #[error(transparent)]
    Ident(#[from] IdentError),
}
