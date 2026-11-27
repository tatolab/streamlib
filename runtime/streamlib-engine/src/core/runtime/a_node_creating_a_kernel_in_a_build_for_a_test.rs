// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node type for tests written as one function whose one computed value creates a compute
//! kernel through the context its build is handed, escalating that context's GPU; each frame
//! tries the same through its own context and records the refusal. One node of the type runs at
//! a time.

use std::sync::Once;

use parking_lot::Mutex;

use crate::core::context::RuntimeContextLimitedAccess;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::Result;
use crate::core::nodes::NODE_REGISTRY;
use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor};
use crate::sdk::nodes::computed;

/// What the node of the type ran, since a test last forgot it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WhatTheKernelCreatingNodeRan {
    /// How many builds created their kernel.
    pub(super) kernels_built: usize,
    /// Why the latest frame's kernel was refused; `None` before any frame.
    pub(super) refusal_in_the_latest_frame: Option<String>,
}

static RAN: Mutex<WhatTheKernelCreatingNodeRan> = Mutex::new(WhatTheKernelCreatingNodeRan {
    kernels_built: 0,
    refusal_in_the_latest_frame: None,
});

fn create_a_kernel(ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
    ctx.gpu_limited_access()
        .escalate(|gpu| {
            gpu.create_compute_kernel(&ComputeKernelDescriptor {
                entry_point: "main",
                label: "kernel_created_by_a_node_function",
                spv: include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/color_convert_nv12_buffer_to_rgba.spv"
                )),
                bindings: &[
                    ComputeBindingSpec::storage_buffer(0),
                    ComputeBindingSpec::storage_image(1),
                ],
                push_constant_size: 96,
                specialization_constants: &[],
            })
        })
        .map(drop)
}

#[crate::sdk::node(execution = continuous(interval_ms = 5))]
fn a_node_creating_a_kernel_in_a_build()
-> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    let kernel_built = computed(move |build_ctx| {
        create_a_kernel(build_ctx)?;
        RAN.lock().kernels_built += 1;
        Ok::<_, crate::core::error::Error>(true)
    });
    move |ctx| {
        let _ = kernel_built.get();
        RAN.lock().refusal_in_the_latest_frame = create_a_kernel(ctx)
            .err()
            .map(|refusal| refusal.to_string());
        Ok(())
    }
}

/// The type of the node creating a kernel in a build, registered once per test binary.
pub(super) fn the_node_type_creating_a_kernel_in_a_build() -> NodeClassImportPath {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<ANodeCreatingAKernelInABuild::Node>());
    *RAN.lock() = WhatTheKernelCreatingNodeRan::default();
    ANodeCreatingAKernelInABuild::Node::node_class_import_path()
}

/// What the node of the type ran.
pub(super) fn what_the_kernel_creating_node_ran() -> WhatTheKernelCreatingNodeRan {
    RAN.lock().clone()
}
