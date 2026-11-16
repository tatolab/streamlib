// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(any())]
#[path = "../processors/_apple_impl_pending_/mod.rs"]
pub mod _apple_impl_pending_;
