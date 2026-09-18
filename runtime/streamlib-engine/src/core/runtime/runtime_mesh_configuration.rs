// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a caller tells a runtime about its place on the runtime mesh.

/// The mesh values a runtime is constructed with, each optional.
///
/// A value left unset here is read from the environment, and failing that takes
/// the engine's own default — so a container configures itself with no code and
/// a Rust app configures itself the same way the wheel does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeMeshConfiguration {
    /// The name this runtime is addressed by on the mesh.
    ///
    /// Unset, the engine reads `STREAMLIB_RUNTIME_NAME`, and failing that names
    /// the runtime after its host, its app directory and that directory's path.
    pub runtime_name: Option<String>,
}
