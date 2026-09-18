// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime mesh — one Zenoh session per runtime, beside iceoryx2.
//!
//! Always on and in the app process: every runtime opens a session when it is
//! constructed, announces itself under its mesh name, discovers the other
//! runtimes with nothing configured, and closes the session when it stops. A
//! helper process opens none, because it never constructs a runtime.

mod host_identity;
mod hosted_control_plane_endpoint;
mod resolved_runtime_mesh_configuration;
mod runtime_mesh_description;
mod runtime_mesh_endpoint;
mod runtime_mesh_key;
mod runtime_mesh_membership;
mod runtime_mesh_name;
mod runtime_mesh_peer_table;

// Exported because something outside this module names it: the runtime and its
// context hold the membership and the control-plane cell, `Runner::new()`
// resolves the configuration, and the platform half of the host identity is in
// `linux/`. Everything else the mesh is built from stays inside it.
pub use host_identity::HostIdentity;
pub use hosted_control_plane_endpoint::HostedControlPlaneEndpointRegistry;
pub use resolved_runtime_mesh_configuration::ResolvedRuntimeMeshConfiguration;
pub use runtime_mesh_membership::RuntimeMeshMembership;
pub use runtime_mesh_name::RuntimeMeshName;
