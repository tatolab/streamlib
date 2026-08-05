// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-DSO half of the surface-adapter contract.
//!
//! The in-process contract every adapter core implements lives in
//! `streamlib-surface-adapter`; this crate carries only what the plugin
//! ABI's dynamic boundary needs. See `docs/architecture/surface-adapter.md`.

pub mod ffi;

/// Major version of the surface-adapter ABI.
///
/// Bumped on a breaking trait change (renamed/removed method, changed
/// signature, changed `#[repr(C)]` field of any associated type).
/// Adding new methods is non-breaking and does NOT bump.
///
/// Rust's vtable layout already enforces in-process compatibility at
/// compile time — a mismatched rlib version can't link into the runtime
/// in the first place. This constant is load-bearing only at the cdylib
/// boundary, where it'll be checked from a `#[repr(C)] AdapterDeclaration`
/// shape (mirroring `streamlib-plugin-abi`'s `PluginDeclaration`) when
/// dynamic adapter loading lands.
pub const STREAMLIB_ADAPTER_ABI_VERSION: u32 = 1;
