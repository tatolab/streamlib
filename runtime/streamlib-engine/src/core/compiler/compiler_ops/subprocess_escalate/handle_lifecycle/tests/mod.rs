// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::EscalateHandleRegistry;
use crate::core::compiler::compiler_ops::subprocess_escalate::WhatAHelperPlacedNodeReaches;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::core::test_support::LiveSurfaceShareServiceForATest;
#[cfg(target_os = "linux")]
use crate::core::test_support::rig_only_capability_skip::{
    RigOnlyCapability, skip_unless_this_machine_has,
};

mod node_output_pool_slot_tests;
mod one_off_acquisition_count_and_memory_budget_tests;
mod tensor_storage_buffer_tests;
#[cfg(target_os = "linux")]
mod texture_export_flavour_tests;

/// The handle id an escalate acquire answered, or its refusal's message.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn handle_id_or_refusal_of(
    response: Option<
        crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse,
    >,
) -> std::result::Result<String, String> {
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;

    match response {
        Some(EscalateResponse::Ok(ok)) => Ok(ok.handle_id),
        Some(EscalateResponse::Err(err)) => Err(err.message),
        None => Err("the escalate op got no response".to_string()),
    }
}

/// Release one handle over the real escalate op, answering the refusal if
/// the release was refused.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn release_a_handle_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    handle_id: &str,
) -> std::result::Result<(), String> {
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;

    handle_id_or_refusal_of(super::super::handle_escalate_op(
        sandbox,
        registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "req-release".to_string(),
            handle_id: handle_id.to_string(),
        }),
    ))
    .map(|_| ())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn next_node_output_frame_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    pool_key: &str,
    rotation_depth: u32,
) -> std::result::Result<String, String> {
    next_node_output_frame_of_width_over_the_escalate_op(
        sandbox,
        registry,
        pool_key,
        rotation_depth,
        64,
    )
}

/// The next 32-high frame `width` wide a node output pool hands out over the
/// real escalate op, or its refusal's message.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn next_node_output_frame_of_width_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    pool_key: &str,
    rotation_depth: u32,
    width: u32,
) -> std::result::Result<String, String> {
    use super::super::handle_escalate_op;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquireTexture, EscalateRequestNodeOutputPool,
    };
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
        EscalateRequest, EscalateResponse,
    };
    use crate::core::rhi::TextureFormat;

    match handle_escalate_op(
        sandbox,
        registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-pooled".to_string(),
            width,
            height: 32,
            format: TextureFormat::Rgba8Unorm.wire_name().to_string(),
            node_output_pool: Some(EscalateRequestNodeOutputPool {
                pool_key: pool_key.to_string(),
                rotation_depth,
            }),
            usage: vec!["texture_binding".to_string()],
        }),
    ) {
        Some(EscalateResponse::Ok(ok)) => Ok(ok.handle_id),
        Some(EscalateResponse::Err(err)) => Err(err.message),
        None => Err("the pooled acquire_texture got no response".to_string()),
    }
}

impl super::NodeOutputPoolsOfOneHelper {
    /// Drain the pools at the helper's teardown, where no reader holds a frame
    /// across it: every slot comes back owed its release.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn drain_at_the_helpers_teardown_with_nothing_held(
        &mut self,
        host: &crate::core::context::GpuContext,
    ) -> Vec<super::ReleasedNodeOutputPoolSlot> {
        let (unheld_slots, slots_a_reader_holds) = self.drain_at_the_helpers_teardown(host);
        assert!(
            slots_a_reader_holds.is_empty(),
            "a slot nothing holds was kept past the helper's teardown"
        );
        unheld_slots
    }
}

#[test]
fn release_handle_flags_unknown_handle() {
    // Registry-level release of an unknown handle. A full
    // integration test that exercises [`handle_escalate_op`]
    // against a real `GpuContextLimitedAccess` lives in the
    // escalate root's `handle_escalate_op_end_to_end` test — it is gated
    // on [`GpuContext::init_for_platform`] succeeding so CI
    // machines without a GPU still build+run the rest of the
    // suite.
    let registry = EscalateHandleRegistry::new();
    assert_eq!(registry.handle_count(), 0);
    assert!(registry.remove_handle("missing").is_none());
}
