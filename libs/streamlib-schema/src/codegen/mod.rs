// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Code generation for schemas.

pub mod python;
pub mod rust;
pub mod typescript;

pub use python::{generate_init_py, generate_python};
pub use rust::{generate_mod_rs, generate_rust};
pub use typescript::{generate_index_ts, generate_typescript};
