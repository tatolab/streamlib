// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Apple display module — ported from the engine but **not compiled**.
//!
//! The Metal-side rewrite onto a Metal-equivalent present target is
//! tracked as a follow-up to #674. The source is retained here for
//! reference; `lib.rs` does NOT declare `pub mod apple;`, so this file
//! never enters the build tree on any target.

pub mod display;
