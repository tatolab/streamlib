// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

mod auth;
mod handlers;
mod processor;
mod state;

pub use _generated_::ApiServerConfig;
pub use processor::ApiServerProcessor;
