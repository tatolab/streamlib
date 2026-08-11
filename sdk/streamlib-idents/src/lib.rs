// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Structured schema identifiers, semver, and channel names.
//!
//! The structured-everywhere rule: identifiers are constructed by codegen or
//! by typed YAML/JSON deserialization. There is no public `parse` API and
//! none should be added — see `docs/architecture/schema-identity-and-packaging.md`.

mod channel;
mod error;
mod ident;
mod semver;

pub use channel::{
    CHANNEL_CHUNK_SEPARATOR, ChannelName, MAX_CHANNEL_NAME_BYTES, source_channel_name,
    validate_channel_name,
};
pub use error::{IdentError, IdentResult};
pub use ident::{
    ModuleIdent, Org, Package, PackageRef, SESSION_ORG, SchemaIdent, TypeName, validate_org,
    validate_package, validate_type,
};
pub use semver::{Prerelease, PrereleaseKind, SemVer, SemVerRange};
