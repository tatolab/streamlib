// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(any(target_os = "macos", target_os = "ios"))]

//! Apple-target refusal arm. Declares no `#[processor]`, so it contributes a
//! `pub mod` to the generated crate root and no `export_plugin!` entry — its
//! whole job is to turn an Apple build into a compile error carrying the
//! reason, instead of a cdylib that loads with no processors and fails at
//! registration with a generic missing-symbol message.

compile_error!(
    "@tatolab/display does not yet build on macOS/iOS — the Metal rewrite onto a Metal \
     present target is tracked as a follow-up. The Apple source is parked under \
     `processors/_apple_impl_pending_/`. Build on Linux today."
);
