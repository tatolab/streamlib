// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(any())]

//! Parked Apple display implementation — the file-level `#![cfg(any())]` above
//! gates the whole directory so it never compiles on any target, which is also
//! what keeps folder-backed discovery from resolving its `#[processor]` into
//! the generated crate root.
//!
//! To unpark, once the Metal-side rewrite onto a Metal-equivalent present
//! target ships: replace the `#![cfg(any())]` with the Apple target predicate
//! and delete `processors/apple_unsupported.rs`, which is what makes an Apple
//! build fail with an actionable message rather than a plugin with no
//! processors.

pub mod display;
