// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Apple AVAssetWriter implementation, preserved verbatim from the
//! engine substrate but **gated off** at the crate root via
//! `#[cfg(any())]`. The module never compiles.
//!
//! Re-enabling this implementation requires the streamlib SDK to
//! expose the Apple platform internals it depends on
//! (`PixelTransferSession`, the `RuntimeContext` Apple-side
//! `run_on_runtime_thread_blocking` workflow). Today those types
//! live in the engine crate but the SDK's Tier-2 `streamlib::sdk::engine`
//! namespace is Linux-cfg-gated, and the project rule for carve-outs
//! is to consume the SDK's public surface exclusively (no
//! `engine_internal` reach-throughs). The carve-out preserves this
//! source so the design work that lands an Apple SDK surface has a
//! concrete target to wire back up.

pub mod mp4_writer;
