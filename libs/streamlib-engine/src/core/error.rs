// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The canonical StreamLib [`Error`] / [`Result`] live in the engine-free
//! `streamlib-error` crate so plugin cdylibs can author against them without
//! linking the engine. The engine re-exports them here unchanged; every
//! `crate::core::error::*` / `crate::core::Error` path resolves through this
//! re-export.
//!
//! Engine-local `From<…> for Error` conversions (`TextureReadbackError`,
//! `AddModuleError`, `RemoveModuleError`) stay next to their source types —
//! the orphan rule permits them because the source type is engine-local even
//! though `Error` is now foreign. The `From<ConsumerRhiError>` conversion
//! moved into `streamlib-error` (both types are engine-foreign, so it cannot
//! live here).

pub use streamlib_error::{Error, PortDirection, Result};
