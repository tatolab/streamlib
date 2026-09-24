// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Windows a helper process owns: minted on the engine's window pump, driven
//! by the engine's present loop, closed by the owner or at teardown.

#[cfg(test)]
mod tests;

use uuid::Uuid;

use super::handle_lifecycle::EscalateHandleRegistry;
use crate::core::color::{ColorTraits, HdrStaticMetadata, PrimariesId, TransferId};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestCreateProcessorOwnedWindow, EscalateRequestShowSurfaceOnProcessorOwnedWindow,
    EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries,
    EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer,
    EscalateRequestShowSurfaceOnProcessorOwnedWindowHdrStaticMetadata,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::Error;
use crate::core::processor_owned_window::{
    ProcessorOwnedWindow, ProcessorOwnedWindowAwaitingItsPresentTarget,
    ProcessorOwnedWindowRequest, SurfaceNamedForTheEnginesPresentLoop,
    WindowPresentLoopForOwningProcessor,
};
use crate::core::window_event_pump::WindowRegistrationRequestFromOwningProcessor;
use crate::host_rhi::PresentScalingMode;

/// Mint a window for a helper process and start the engine's present loop on
/// it, answering with the id every other present-class op names.
///
/// The pump round trip happens outside the escalate gate, for the reason
/// [`ProcessorOwnedWindowAwaitingItsPresentTarget`] carries.
pub(super) fn handle_create_processor_owned_window(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    request_id: String,
    request: EscalateRequestCreateProcessorOwnedWindow,
) -> EscalateResponse {
    if !registry.the_last_lifecycle_command_sent_to_the_helper_process_was_setup() {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id,
            message: format!(
                "create_processor_owned_window is a setup-phase request: the window titled \
                 {:?} was asked for outside this processor's setup() hook, and a window is \
                 never minted mid-process()",
                request.window_title
            ),
        });
    }

    let window_title = request.window_title.clone();
    let registered_window =
        match ProcessorOwnedWindowAwaitingItsPresentTarget::register_on_the_process_wide_window_event_pump(
            ProcessorOwnedWindowRequest {
                window_registration_request: WindowRegistrationRequestFromOwningProcessor {
                    window_title: request.window_title,
                    initial_width_in_logical_pixels: request.initial_width_in_logical_pixels,
                    initial_height_in_logical_pixels: request.initial_height_in_logical_pixels,
                },
                // Letterbox, and no dial on the request: the compositor's
                // default is the decided behaviour, and a scaling mode is
                // additive surface rather than plan text.
                scaling_mode_for_frame_in_window: PresentScalingMode::Fit,
            },
        ) {
            Ok(registered_window) => registered_window,
            Err(e) => {
                return EscalateResponse::Err(processor_owned_window_could_not_be_minted_error(
                    request_id,
                    &window_title,
                    &e,
                ));
            }
        };

    let processor_owned_window = sandbox.escalate(|full| {
        ProcessorOwnedWindow::open_present_target_for_registered_window(full, registered_window)
    });
    let processor_owned_window = match processor_owned_window {
        Ok(processor_owned_window) => processor_owned_window,
        Err(e) => {
            return EscalateResponse::Err(processor_owned_window_could_not_be_minted_error(
                request_id,
                &window_title,
                &e,
            ));
        }
    };

    let (width, height) = processor_owned_window.current_extent_in_physical_pixels();
    let present_loop = match WindowPresentLoopForOwningProcessor::start_for_processor_owned_window(
        processor_owned_window,
    ) {
        Ok(present_loop) => present_loop,
        Err(e) => {
            return EscalateResponse::Err(processor_owned_window_could_not_be_minted_error(
                request_id,
                &window_title,
                &e,
            ));
        }
    };
    let window_id = format!("processor-owned-window-{}", Uuid::new_v4());
    registry.insert_processor_owned_window(window_id.clone(), present_loop);
    EscalateResponse::Ok(EscalateResponseOk {
        request_id,
        handle_id: window_id,
        width: Some(width),
        height: Some(height),
        processor_owned_window_is_closed: Some(false),
        ..Default::default()
    })
}

/// What one `show_surface_on_processor_owned_window` amounted to, once the
/// window it named had been found. A window that was never found is the
/// caller's own refusal, not one of these.
pub(super) enum ShowSurfaceOnProcessorOwnedWindowOutcome {
    /// Handed to the window's present loop, which shows it at the next vsync
    /// unless a newer id lands first.
    NamedForTheNextPresent,
    /// The engine had already closed the window, so nothing was named. The
    /// op's answer to a user gesture, and deliberately not an error.
    WindowIsClosedSoNothingWasNamed,
    /// The id names a frame whose pool slot has been recycled since, carrying
    /// the recycling's own account of it.
    SurfaceIdWasRetired(String),
}

/// Name the frame a window shows next, without waiting for it to be shown.
///
/// No escalate gate: the gate serialises runtime-wide and waits for device
/// idle, and this is a per-frame op that starts no GPU work of its own — the
/// window's own thread does the acquiring and composing.
///
/// The closed window is answered before the surface id is judged: after a
/// close this op is a no-op that reports closed, and a stale id in the same
/// call must not turn that into the error the close was never allowed to be.
pub(super) fn handle_show_surface_on_processor_owned_window(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    request_id: String,
    request: EscalateRequestShowSurfaceOnProcessorOwnedWindow,
) -> EscalateResponse {
    let Some(present_loop) = registry.processor_owned_window(&request.window_id) else {
        return EscalateResponse::Err(unknown_processor_owned_window_error(
            request_id,
            "show_surface_on_processor_owned_window",
            &request.window_id,
        ));
    };

    let outcome = if present_loop.window_is_closed() {
        ShowSurfaceOnProcessorOwnedWindowOutcome::WindowIsClosedSoNothingWasNamed
    } else if let Err(e) = sandbox
        .host_inner()
        .refuse_a_retired_frame_id(&request.surface_id)
    {
        // Refused here rather than left to the loop: the owner is owed the
        // recycling by name at the call that got it wrong, never a window
        // that quietly keeps its last frame. Judged after the closed window,
        // because a stale id must not turn the close's no-op into an error.
        ShowSurfaceOnProcessorOwnedWindowOutcome::SurfaceIdWasRetired(e.to_string())
    } else {
        present_loop
            .name_surface_for_the_next_present(surface_named_for_the_present_loop_of(&request));
        ShowSurfaceOnProcessorOwnedWindowOutcome::NamedForTheNextPresent
    };
    let window_id = request.window_id;

    match outcome {
        ShowSurfaceOnProcessorOwnedWindowOutcome::NamedForTheNextPresent => {
            EscalateResponse::Ok(EscalateResponseOk {
                request_id,
                handle_id: window_id,
                processor_owned_window_is_closed: Some(false),
                ..Default::default()
            })
        }
        ShowSurfaceOnProcessorOwnedWindowOutcome::WindowIsClosedSoNothingWasNamed => {
            EscalateResponse::Ok(EscalateResponseOk {
                request_id,
                handle_id: window_id,
                processor_owned_window_is_closed: Some(true),
                ..Default::default()
            })
        }
        ShowSurfaceOnProcessorOwnedWindowOutcome::SurfaceIdWasRetired(recycling) => {
            EscalateResponse::Err(EscalateResponseErr {
                request_id,
                message: format!("show_surface_on_processor_owned_window refused: {recycling}"),
            })
        }
    }
}

/// Hand the owner its window's coalesced state: current extent, whether the
/// user asked to close it since the last drain, and whether the engine has
/// closed it.
pub(super) fn handle_drain_processor_owned_window_events(
    registry: &EscalateHandleRegistry,
    request_id: String,
    window_id: String,
) -> EscalateResponse {
    let drained = registry
        .processor_owned_window(&window_id)
        .map(|present_loop| present_loop.drain_coalesced_state_for_the_owning_processor());
    match drained {
        Some(drained) => EscalateResponse::Ok(EscalateResponseOk {
            request_id,
            handle_id: window_id,
            width: Some(drained.current_width_in_physical_pixels),
            height: Some(drained.current_height_in_physical_pixels),
            close_requested_by_user: Some(drained.close_requested_by_user),
            processor_owned_window_is_closed: Some(drained.window_is_closed),
            ..Default::default()
        }),
        None => EscalateResponse::Err(unknown_processor_owned_window_error(
            request_id,
            "drain_processor_owned_window_events",
            &window_id,
        )),
    }
}

/// Release a window the owner is done with: the present thread stops and is
/// waited for, and dropping the window's pump registration is what closes it.
///
/// The GPU work this runs is the present target's own drop — device-idle,
/// then destroy its swapchain and surface — which orders itself against
/// nothing but that target, so it needs no runtime-wide escalate gate. That
/// is why the op takes none, not because closing is free.
///
/// The id stays this processor's until teardown, closed. One answer for a
/// closed window however it closed — a user gesture and an owner's own close
/// both leave `show…` a no-op reporting closed, rather than making the second
/// one an error the first was never allowed to be. The answer reports what is
/// true: a window server still holding the present thread past the close's
/// grace window leaves the window open, and says so.
pub(super) fn handle_close_processor_owned_window(
    registry: &EscalateHandleRegistry,
    request_id: String,
    window_id: String,
) -> EscalateResponse {
    match registry
        .processor_owned_window(&window_id)
        .map(|present_loop| present_loop.close_the_window_and_join_its_present_thread())
    {
        Some(window_is_closed) => EscalateResponse::Ok(EscalateResponseOk {
            request_id,
            handle_id: window_id,
            processor_owned_window_is_closed: Some(window_is_closed),
            ..Default::default()
        }),
        None => EscalateResponse::Err(unknown_processor_owned_window_error(
            request_id,
            "close_processor_owned_window",
            &window_id,
        )),
    }
}

/// The frame one request names, projected onto what the engine's present
/// loop takes — the whole of it, so a field dropped on the way through is a
/// test failure rather than a window quietly showing an undescribed frame.
///
/// Borrows and clones the id rather than consuming the request, because the
/// response still owes the caller its window id; one short-string clone on a
/// path that has just decoded this document from JSON.
pub(super) fn surface_named_for_the_present_loop_of(
    request: &EscalateRequestShowSurfaceOnProcessorOwnedWindow,
) -> SurfaceNamedForTheEnginesPresentLoop {
    SurfaceNamedForTheEnginesPresentLoop {
        surface_id: request.surface_id.clone(),
        source_width_in_pixels: request.source_width_in_pixels,
        source_height_in_pixels: request.source_height_in_pixels,
        producer_published_texture_layout: request.producer_published_texture_layout,
        color_traits_of_frame: color_traits_of_frame_named_over_the_wire(
            request.color_primaries_of_frame,
            request.color_transfer_of_frame,
        ),
        hdr_static_metadata_of_frame: request
            .hdr_static_metadata_of_frame
            .map(hdr_static_metadata_named_over_the_wire),
    }
}

/// The frame's colour description, or `None` when the caller named neither
/// axis.
///
/// Naming either axis alone is a description: the seam resolves the absent
/// one itself, and answering `Some` with both axes empty would renegotiate
/// the swapchain to the default pick rather than leave the window alone.
pub(super) fn color_traits_of_frame_named_over_the_wire(
    color_primaries_of_frame: Option<
        EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries,
    >,
    color_transfer_of_frame: Option<EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer>,
) -> Option<ColorTraits> {
    if color_primaries_of_frame.is_none() && color_transfer_of_frame.is_none() {
        return None;
    }
    Some(ColorTraits {
        primaries: color_primaries_of_frame.map(|primaries| match primaries {
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Bt709 => {
                PrimariesId::Bt709
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Bt470M => {
                PrimariesId::Bt470M
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Bt470Bg => {
                PrimariesId::Bt470Bg
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Smpte170m => {
                PrimariesId::Smpte170m
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Smpte240m => {
                PrimariesId::Smpte240m
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Film => {
                PrimariesId::Film
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Bt2020 => {
                PrimariesId::Bt2020
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Smpte428 => {
                PrimariesId::Smpte428
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Smpte431 => {
                PrimariesId::Smpte431
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Smpte432 => {
                PrimariesId::Smpte432
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries::Ebu3213 => {
                PrimariesId::Ebu3213
            }
        }),
        transfer: color_transfer_of_frame.map(|transfer| match transfer {
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Linear => {
                TransferId::Linear
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Srgb => TransferId::Srgb,
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Bt709 => {
                TransferId::Bt709
            }
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Pq => TransferId::Pq,
            EscalateRequestShowSurfaceOnProcessorOwnedWindowColorTransfer::Hlg => TransferId::Hlg,
        }),
    })
}

/// The frame's HDR sidecar, field for field: the wire already carries the f32
/// units the driver takes, so this converts nothing and only crosses types.
pub(super) fn hdr_static_metadata_named_over_the_wire(
    hdr_static_metadata_of_frame: EscalateRequestShowSurfaceOnProcessorOwnedWindowHdrStaticMetadata,
) -> HdrStaticMetadata {
    HdrStaticMetadata {
        display_primary_red: hdr_static_metadata_of_frame.display_primary_red,
        display_primary_green: hdr_static_metadata_of_frame.display_primary_green,
        display_primary_blue: hdr_static_metadata_of_frame.display_primary_blue,
        white_point: hdr_static_metadata_of_frame.white_point,
        min_luminance_cd_m2: hdr_static_metadata_of_frame.min_luminance_cd_m2,
        max_luminance_cd_m2: hdr_static_metadata_of_frame.max_luminance_cd_m2,
        max_content_light_level: hdr_static_metadata_of_frame.max_content_light_level,
        max_frame_average_light_level: hdr_static_metadata_of_frame.max_frame_average_light_level,
    }
}

/// The one refusal for a window this process could not get — no display
/// server, a dead pump, a present target that would not mint. It carries the
/// cause's own account rather than substituting for it, because that is what
/// tells a Python author whether to handle the refusal or fix the call.
pub(super) fn processor_owned_window_could_not_be_minted_error(
    request_id: String,
    window_title: &str,
    cause: &Error,
) -> EscalateResponseErr {
    EscalateResponseErr {
        request_id,
        message: format!(
            "create_processor_owned_window failed for the window titled {window_title:?}: {cause}"
        ),
    }
}

/// The one refusal for an id that names no window this subprocess owns —
/// never minted, or already released.
pub(super) fn unknown_processor_owned_window_error(
    request_id: String,
    op_wire_name: &str,
    window_id: &str,
) -> EscalateResponseErr {
    EscalateResponseErr {
        request_id,
        message: format!(
            "{op_wire_name}: window_id '{window_id}' names no window this processor owns"
        ),
    }
}
