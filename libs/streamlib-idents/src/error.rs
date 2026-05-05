// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
