// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime mesh — one Zenoh session per runtime, beside iceoryx2.
//!
//! Always on and in the app process: every runtime opens a session when it is
//! constructed, announces itself under its mesh name, discovers the other
//! runtimes with nothing configured, and closes the session when it stops. A
//! helper process opens none, because it never constructs a runtime.
//!
//! A process that only wants to *look* at a mesh joins none: `streamlib
//! nodes` reads it through [`observe_a_runtime_mesh`], on a session that
//! announces nothing.

mod a_bags_top_level_surface_id;
mod a_frames_pixels_on_the_mesh;
mod a_frames_pixels_read_out_for_the_mesh;
mod a_frames_pixels_written_into_a_local_surface;
mod duplicate_runtime_name_on_the_mesh;
mod gpu_context_the_mesh_copies_frames_with;
mod host_identity;
mod hosted_control_plane_endpoint;
mod link_request_on_the_mesh;
mod link_requests_from_other_runtimes;
mod link_requests_this_runtime_has_sent;
mod machine_clock_identity;
mod mesh_data_message_attachment;
mod mesh_link_ingress;
mod mesh_link_ingress_table;
mod mesh_port_egress;
mod mesh_port_egress_table;
mod output_ports_offered_on_the_mesh;
mod output_ports_other_runtimes_are_reading;
mod resolved_runtime_mesh_configuration;
mod runtime_mesh_description;
mod runtime_mesh_endpoint;
mod runtime_mesh_key;
mod runtime_mesh_membership;
mod runtime_mesh_name;
mod runtime_mesh_observation;
mod runtime_mesh_peer_table;
mod zenoh_work_off_any_tokio_runtime;

// Exported because something outside this module names it: the runtime and its
// context hold the membership and the control-plane cell, `Runner::new()`
// resolves the configuration, and the platform halves of the host identity and the
// machine clock identity are in `linux/` and `apple/`. The announced identity and the key space are exported for the
// mesh's own two-process fixture, which has to write the very key the
// duplicate-name check reads rather than re-spell the grammar beside it.
// The observation is exported because the wheel's `streamlib nodes` door
// calls it. Everything else the mesh is built from stays inside it.
pub use gpu_context_the_mesh_copies_frames_with::GpuContextTheMeshCopiesFramesWith;
pub use host_identity::HostIdentity;
pub use hosted_control_plane_endpoint::HostedControlPlaneEndpointRegistry;
pub use link_request_on_the_mesh::{
    ALinkRequestOnTheMesh, WhatALinkRequestAsksFor, WhatALinkRequestWasAnswered,
    WhichOperationALinkRequestNames, WhyALinkRequestWasRefused,
};
pub use link_requests_from_other_runtimes::{
    HowARuntimeAnsweredALinkRequest, WhatThisRuntimeDoesWithALinkRequest,
    WhatThisRuntimeDoesWithALinkRequestRegistry,
};
pub use link_requests_this_runtime_has_sent::LinkRequestsThisRuntimeHasSent;
pub use machine_clock_identity::MachineClockIdentity;
pub use mesh_data_message_attachment::{
    MESH_DATA_MESSAGE_ATTACHMENT_BYTES, MeshDataMessageAttachment,
};
#[doc(hidden)]
pub use mesh_link_ingress_table::MeshLinkIngressTable;
// Reachable rather than supported, like the key grammar above: the
// cross-runtime-link fixture stands two runtimes' mesh halves up without a
// `Runner`, because CI has no GPU to start one with.
#[doc(hidden)]
pub use output_ports_offered_on_the_mesh::{
    HowToReadAnOfferedOutputPort, WhatThisRuntimeOffersOnTheMesh,
    WhatThisRuntimeOffersOnTheMeshRegistry,
};
pub use output_ports_offered_on_the_mesh::{
    OutputPortOfferedOnTheMesh, OutputPortThisRuntimeHoldsAndCannotSend,
    OutputPortsOfferedOnTheMesh,
};
pub use resolved_runtime_mesh_configuration::ResolvedRuntimeMeshConfiguration;
// Reachable rather than supported: `core::runtime` is a public module, and the
// key grammar is the mesh's own business.
#[doc(hidden)]
pub use runtime_mesh_key::{AnnouncedRuntimeIdentity, RuntimeMeshKeySpace};
pub use runtime_mesh_membership::RuntimeMeshMembership;
pub use runtime_mesh_name::RuntimeMeshName;
pub use runtime_mesh_observation::{
    RuntimeMeshObservation, RuntimeMeshObservationRequest, observe_a_runtime_mesh,
};
