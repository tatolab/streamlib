// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The present-class ops' own gates: which lifecycle hook may mint a
//! window, and what every op answers for a window nobody owns.

use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::super::{handle_escalate_op, request_id};
use super::{
    color_traits_of_frame_named_over_the_wire, handle_close_processor_owned_window,
    handle_drain_processor_owned_window_events, hdr_static_metadata_named_over_the_wire,
    surface_named_for_the_present_loop_of,
};
use crate::core::color::{ColorTraits, HdrStaticMetadata, PrimariesId, TransferId};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestCloseProcessorOwnedWindow, EscalateRequestCreateProcessorOwnedWindow,
    EscalateRequestDrainProcessorOwnedWindowEvents,
    EscalateRequestShowSurfaceOnProcessorOwnedWindow,
    EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries,
    EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer,
    EscalateRequestShowSurfaceOnProcessorOwnedWindowHdrStaticMetadata,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
    EscalateRequest, EscalateResponse,
};
use crate::core::context::{GpuContext, GpuContextLimitedAccess};
use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

const A_WINDOW_ID_NOBODY_OWNS: &str = "processor-owned-window-never-minted";

fn sandbox_or_skip(test_name: &str) -> Option<GpuContextLimitedAccess> {
    match GpuContext::init_for_platform_sync() {
        Ok(gpu) => Some(GpuContextLimitedAccess::new(gpu)),
        Err(e) => {
            println!("{test_name}: no GPU device ({e}) — skipping");
            None
        }
    }
}

/// A refusal a person reads, so a hand-wrapped format string that
/// lost its line continuation is a defect rather than cosmetics —
/// and one no other gate catches, because `cargo fmt` does not
/// reflow string literals and clippy does not read them.
fn assert_no_collapsed_whitespace(message: &str) {
    assert!(
        !message.contains("  "),
        "the refusal carries a run of literal spaces, so a line continuation was lost \
         when it was wrapped: {message:?}"
    );
}

fn refusal_message_of(response: EscalateResponse, expected_request_id: &str) -> String {
    match response {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, expected_request_id);
            err.message
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A window is a setup-phase resource request, so the engine decides
/// it — not the wheel's typestate. Both Python capability tiers
/// collapse onto this one wire, so which Python object carried the
/// call is invisible by the time the op arrives, and a guard that
/// lived only in the child would be no guard at all.
#[test]
fn only_the_setup_hook_may_mint_a_processor_owned_window() {
    let registry = EscalateHandleRegistry::new();
    assert!(
        !registry.the_last_lifecycle_command_sent_to_the_helper_process_was_setup(),
        "a request arriving before the parent has sent anything is inside no hook"
    );

    registry.note_lifecycle_command_sent_to_the_helper_process("setup");
    assert!(
        registry.the_last_lifecycle_command_sent_to_the_helper_process_was_setup(),
        "the setup hook is the one place a window may be asked for"
    );

    for command_outside_the_setup_hook in [
        "run",
        "stop",
        "teardown",
        "on_pause",
        "on_resume",
        "update_config",
    ] {
        registry.note_lifecycle_command_sent_to_the_helper_process(command_outside_the_setup_hook);
        assert!(
            !registry.the_last_lifecycle_command_sent_to_the_helper_process_was_setup(),
            "{command_outside_the_setup_hook} is not the setup hook, so a window minted \
             from it would be minted mid-pipeline"
        );
    }
}

/// Two ops need no GPU capability to answer, so their refusal is
/// checked wherever the suite runs: an id that names no window of
/// this processor's is refused by name, and never mistaken for the
/// different failure of having no display server.
#[test]
fn drain_and_close_refuse_a_window_this_processor_does_not_own_by_name() {
    let registry = EscalateHandleRegistry::new();

    let refusals = [
        (
            "drain_processor_owned_window_events",
            handle_drain_processor_owned_window_events(
                &registry,
                "req-drain".to_string(),
                A_WINDOW_ID_NOBODY_OWNS.to_string(),
            ),
            "req-drain",
        ),
        (
            "close_processor_owned_window",
            handle_close_processor_owned_window(
                &registry,
                "req-close".to_string(),
                A_WINDOW_ID_NOBODY_OWNS.to_string(),
            ),
            "req-close",
        ),
    ];

    for (op_wire_name, response, expected_request_id) in refusals {
        let message = refusal_message_of(response, expected_request_id);
        assert!(
            message.contains(op_wire_name) && message.contains(A_WINDOW_ID_NOBODY_OWNS),
            "{op_wire_name}: the refusal must name the op and the window id, got: {message}"
        );
        assert!(
            !message.contains("display"),
            "{op_wire_name}: an unowned window is not a missing display server, got: \
             {message}"
        );
        assert_no_collapsed_whitespace(&message);
    }
}

/// The same refusal, reached the way a helper reaches it — through
/// the dispatch arm, with the request decoded from its wire variant.
#[test]
fn every_present_class_op_refuses_a_window_this_processor_does_not_own() {
    let Some(sandbox) =
        sandbox_or_skip("every_present_class_op_refuses_a_window_this_processor_does_not_own")
    else {
        return;
    };
    let registry = EscalateHandleRegistry::new();

    let requests = [
        EscalateRequest::ShowSurfaceOnProcessorOwnedWindow(
            EscalateRequestShowSurfaceOnProcessorOwnedWindow {
                request_id: "req-show".into(),
                window_id: A_WINDOW_ID_NOBODY_OWNS.into(),
                surface_id: "a-surface-this-process-never-saw".into(),
                source_width_in_pixels: 64,
                source_height_in_pixels: 64,
                color_primaries_of_frame: None,
                color_transfer_of_frame: None,
                hdr_static_metadata_of_frame: None,
                producer_published_texture_layout: None,
            },
        ),
        EscalateRequest::DrainProcessorOwnedWindowEvents(
            EscalateRequestDrainProcessorOwnedWindowEvents {
                request_id: "req-drain".into(),
                window_id: A_WINDOW_ID_NOBODY_OWNS.into(),
            },
        ),
        EscalateRequest::CloseProcessorOwnedWindow(EscalateRequestCloseProcessorOwnedWindow {
            request_id: "req-close".into(),
            window_id: A_WINDOW_ID_NOBODY_OWNS.into(),
        }),
    ];

    for request in requests {
        let expected_request_id = request_id(&request)
            .expect("every present-class op carries a correlation token")
            .to_string();
        let response = handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            request,
        )
        .expect("every present-class op produces a response");
        let message = refusal_message_of(response, &expected_request_id);
        assert!(
            message.contains(A_WINDOW_ID_NOBODY_OWNS),
            "{expected_request_id}: the refusal must name the window id, got: {message}"
        );
        assert_no_collapsed_whitespace(&message);
    }
}

/// The whole projection from one wire request onto what the present
/// loop takes. Every field asserted concretely, so dropping any one
/// on the way through fails here rather than showing an undescribed
/// frame on a window nobody is watching in CI.
#[test]
fn a_wire_request_projects_onto_the_frame_the_present_loop_shows() {
    let named =
        surface_named_for_the_present_loop_of(&EscalateRequestShowSurfaceOnProcessorOwnedWindow {
            request_id: "req-show".into(),
            window_id: A_WINDOW_ID_NOBODY_OWNS.into(),
            surface_id: "pool-slot-7#3".into(),
            source_width_in_pixels: 1920,
            source_height_in_pixels: 1080,
            color_primaries_of_frame: Some(
                EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Bt2020,
            ),
            color_transfer_of_frame: Some(
                EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Pq,
            ),
            hdr_static_metadata_of_frame: Some(
                EscalateRequestShowSurfaceOnProcessorOwnedWindowHdrStaticMetadata {
                    display_primary_red: [0.708, 0.292],
                    display_primary_green: [0.170, 0.797],
                    display_primary_blue: [0.131, 0.046],
                    white_point: [0.3127, 0.3290],
                    min_luminance_cd_m2: 0.005,
                    max_luminance_cd_m2: 1000.0,
                    max_content_light_level: 1000.0,
                    max_frame_average_light_level: 400.0,
                },
            ),
            producer_published_texture_layout: Some(1000001002),
        });

    assert_eq!(named.surface_id, "pool-slot-7#3");
    assert_eq!(named.source_width_in_pixels, 1920);
    assert_eq!(named.source_height_in_pixels, 1080);
    assert_eq!(named.producer_published_texture_layout, Some(1000001002));
    assert_eq!(
        named.color_traits_of_frame,
        Some(ColorTraits {
            primaries: Some(PrimariesId::Bt2020),
            transfer: Some(TransferId::Pq),
        }),
        "the colour a helper named must reach the seam that renegotiates on it"
    );
    assert_eq!(
        named
            .hdr_static_metadata_of_frame
            .map(|hdr| hdr.white_point),
        Some([0.3127, 0.3290]),
        "the HDR sidecar must reach the seam that signals it"
    );
}

/// A frame's colour description survives the hop: either axis alone
/// is a description, and naming neither is silence rather than a
/// request for the default pick — which would renegotiate the
/// window's swapchain on every undescribed frame.
#[test]
fn a_frames_colour_description_crosses_the_wire_onto_the_engines_own_ids() {
    assert_eq!(
        color_traits_of_frame_named_over_the_wire(None, None),
        None,
        "an undescribed frame must leave the window on what it last applied"
    );
    assert_eq!(
        color_traits_of_frame_named_over_the_wire(
            Some(EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Bt2020),
            Some(EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Pq),
        ),
        Some(ColorTraits {
            primaries: Some(PrimariesId::Bt2020),
            transfer: Some(TransferId::Pq),
        }),
        "HDR10's two axes are exactly what the seam renegotiates on"
    );
    assert_eq!(
        color_traits_of_frame_named_over_the_wire(
            None,
            Some(EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Hlg),
        ),
        Some(ColorTraits {
            primaries: None,
            transfer: Some(TransferId::Hlg),
        }),
        "one axis alone is a description — the seam resolves the other itself"
    );
}

/// The HDR sidecar crosses field for field, in the f32 units the
/// driver takes. A transposition here shows up as a mastering display
/// the window believes has the wrong primaries.
#[test]
fn the_hdr_sidecar_crosses_the_wire_field_for_field() {
    let crossed = hdr_static_metadata_named_over_the_wire(
        EscalateRequestShowSurfaceOnProcessorOwnedWindowHdrStaticMetadata {
            display_primary_red: [0.708, 0.292],
            display_primary_green: [0.170, 0.797],
            display_primary_blue: [0.131, 0.046],
            white_point: [0.3127, 0.3290],
            min_luminance_cd_m2: 0.005,
            max_luminance_cd_m2: 1000.0,
            max_content_light_level: 1000.0,
            max_frame_average_light_level: 400.0,
        },
    );
    assert_eq!(
        crossed,
        HdrStaticMetadata {
            display_primary_red: [0.708, 0.292],
            display_primary_green: [0.170, 0.797],
            display_primary_blue: [0.131, 0.046],
            white_point: [0.3127, 0.3290],
            min_luminance_cd_m2: 0.005,
            max_luminance_cd_m2: 1000.0,
            max_content_light_level: 1000.0,
            max_frame_average_light_level: 400.0,
        },
        "BT.2020 mastering metadata must reach the driver unpermuted"
    );
}

/// Asked for from anywhere but `setup()`, a window is refused with
/// the phase named and nothing minted — the plan's "requested in
/// setup() … never minted mid-process()", enforced where both Python
/// tiers land rather than in the object that carried the call.
#[test]
fn a_window_asked_for_outside_the_setup_hook_is_refused_and_none_is_minted() {
    let Some(sandbox) =
        sandbox_or_skip("a_window_asked_for_outside_the_setup_hook_is_refused_and_none_is_minted")
    else {
        return;
    };
    let registry = EscalateHandleRegistry::new();
    registry.note_lifecycle_command_sent_to_the_helper_process("run");

    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::CreateProcessorOwnedWindow(EscalateRequestCreateProcessorOwnedWindow {
            request_id: "req-create".into(),
            window_title: "a window asked for mid-pipeline".into(),
            initial_width_in_logical_pixels: 320,
            initial_height_in_logical_pixels: 240,
        }),
    )
    .expect("create_processor_owned_window produces a response");

    let message = refusal_message_of(response, "req-create");
    assert!(
        message.contains("setup") && message.contains("a window asked for mid-pipeline"),
        "the refusal must name the phase and the window, got: {message}"
    );
    assert_no_collapsed_whitespace(&message);
    assert!(
        registry.drain_processor_owned_windows().is_empty(),
        "a refused request must leave no window behind"
    );
}
