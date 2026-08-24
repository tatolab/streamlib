// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The four present-class escalate ops, driven from the helper's own end of
//! the escalate socket: length-prefixed JSON frames, one correlation id per
//! request — the same documents `streamlib/_helper.py` builds. Nothing here
//! reaches into the engine's crate-private dispatch, so what passes is the
//! wire a helper process actually speaks.
//!
//! Its own process, because minting a window over this wire mints the
//! process's one event loop and a Vulkan device alongside it. What the
//! engine's loop does with a named surface is
//! `processor_owned_window_present_loop`.
//!
//! Display tier: needs a display server and a GPU, so it runs on the rig
//! only.

#![cfg(target_os = "linux")]

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use streamlib_engine::core::context::{GpuContext, GpuContextLimitedAccess};
use streamlib_engine::core::helper_process_transport::SubprocessBridge;
use streamlib_engine::core::rhi::{PixelFormat, TextureFormat};
use streamlib_engine::core::window_event_pump::process_wide_window_event_pump;

mod helper_process_escalate_socket;
use helper_process_escalate_socket::{
    HelperProcessEndOfTheEscalateSocket, refusal_message_of,
    send_lifecycle_command_and_let_the_helper_read_it,
};

const SOURCE_EXTENT_IN_PIXELS: u32 = 256;

/// A window id in the shape the create op mints, that no processor owns.
const A_WINDOW_ID_NOBODY_OWNS: &str = "processor-owned-window-never-minted";

fn assert_ok(response: &Value, what_was_asked: &str) {
    assert_eq!(
        response["result"],
        json!("ok"),
        "{what_was_asked} must be answered ok, got {response}"
    );
}

fn show_surface_on_window(surface_id: &str, window_id: &str) -> Value {
    json!({
        "op": "show_surface_on_processor_owned_window",
        "window_id": window_id,
        "surface_id": surface_id,
        "source_width_in_pixels": SOURCE_EXTENT_IN_PIXELS,
        "source_height_in_pixels": SOURCE_EXTENT_IN_PIXELS,
    })
}

/// The same frame, described the way a Python owner reading an HDR10 bag
/// would describe it: BT.2020 primaries, PQ transfer, and the mastering
/// display's sidecar in the f32 units the driver takes.
fn show_hdr_surface_on_window(surface_id: &str, window_id: &str) -> Value {
    let mut op = show_surface_on_window(surface_id, window_id);
    op["color_primaries_of_frame"] = json!("bt2020");
    op["color_transfer_of_frame"] = json!("pq");
    op["hdr_static_metadata_of_frame"] = json!({
        "display_primary_red": [0.708, 0.292],
        "display_primary_green": [0.170, 0.797],
        "display_primary_blue": [0.131, 0.046],
        "white_point": [0.3127, 0.3290],
        "min_luminance_cd_m2": 0.005,
        "max_luminance_cd_m2": 1000.0,
        "max_content_light_level": 1000.0,
        "max_frame_average_light_level": 400.0,
    });
    op
}

fn create_window_titled(window_title: &str) -> Value {
    json!({
        "op": "create_processor_owned_window",
        "window_title": window_title,
        "initial_width_in_physical_pixels": 320,
        "initial_height_in_physical_pixels": 240,
    })
}

/// A published frame id whose pool slot has been rehanded since — the
/// recycled-frame case the surface-id lifetime contract exists for.
///
/// Cycles the pixel-buffer pool, dropping each acquisition immediately so
/// nothing but the pool's own reuse decides when the first slot comes back.
fn a_retired_frame_id(gpu_context_limited_access: &GpuContextLimitedAccess) -> String {
    fn acquire_one_pool_slot_id(gpu_context_limited_access: &GpuContextLimitedAccess) -> String {
        gpu_context_limited_access
            .escalate(|gpu_context_full_access| {
                gpu_context_full_access
                    .acquire_pixel_buffer(
                        SOURCE_EXTENT_IN_PIXELS,
                        SOURCE_EXTENT_IN_PIXELS,
                        PixelFormat::Rgba32,
                    )
                    // Dropped here on purpose: the slot is free as far as the
                    // in-process refcount goes, so only the pool decides when
                    // it is rehanded.
                    .map(|(published_frame_id, _returned_to_the_pool)| {
                        published_frame_id.to_string()
                    })
            })
            .expect("a pixel buffer acquires")
    }

    fn pool_slot_of(published_frame_id: &str) -> &str {
        published_frame_id
            .rsplit_once('#')
            .expect("a pooled acquisition publishes <slot>#<generation>")
            .0
    }

    let first_published_frame_id = acquire_one_pool_slot_id(gpu_context_limited_access);
    for _ in 0..16 {
        let later_published_frame_id = acquire_one_pool_slot_id(gpu_context_limited_access);
        if pool_slot_of(&later_published_frame_id) == pool_slot_of(&first_published_frame_id)
            && later_published_frame_id != first_published_frame_id
        {
            return first_published_frame_id;
        }
    }
    panic!("cycling the pool never rehanded the slot {first_published_frame_id} was minted from");
}

fn wait_until_the_pump_routes_to_exactly(expected_window_count: usize) {
    let pump = process_wide_window_event_pump().expect("the pump minted this test's windows");
    // The pump's count lags a registration's drop by the round trip its
    // deregistration takes through the event loop, so this polls.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if pump.registered_window_count() == expected_window_count {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "the pump routes to {} windows, expected {expected_window_count}",
        pump.registered_window_count()
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "display tier — needs a display server ($DISPLAY / $WAYLAND_DISPLAY) + a GPU. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
#[test]
fn a_helper_process_mints_names_polls_and_closes_a_window_entirely_over_the_wire() {
    let gpu_context = GpuContext::init_for_platform().expect("a GPU is required for this tier");
    let gpu_context_limited_access = gpu_context.limited_access();

    let (parent_end, helper_end) =
        UnixStream::pair().expect("a socketpair stands in for the spawned helper's");
    let bridge = SubprocessBridge::new(
        parent_end,
        gpu_context_limited_access.clone(),
        "processor-owned-window-over-the-wire".to_string(),
    )
    .expect("the bridge wraps the parent end");
    let mut helper = HelperProcessEndOfTheEscalateSocket::new(helper_end);

    let published_surface_id = gpu_context_limited_access
        .escalate(|gpu_context_full_access| {
            gpu_context_full_access.acquire_output_texture(
                SOURCE_EXTENT_IN_PIXELS,
                SOURCE_EXTENT_IN_PIXELS,
                TextureFormat::Bgra8Unorm,
            )
        })
        .expect("a surface to name");
    let retired_frame_id = a_retired_frame_id(&gpu_context_limited_access);

    send_lifecycle_command_and_let_the_helper_read_it(&bridge, &mut helper, "setup");

    // Minting.
    let created = helper
        .escalate_request_to_the_parent(create_window_titled("streamlib window over the wire"));
    assert_ok(&created, "create_processor_owned_window");
    let window_id = created["handle_id"]
        .as_str()
        .expect("the create names the window")
        .to_string();
    assert_eq!(
        created["processor_owned_window_is_closed"],
        json!(false),
        "a freshly minted window is not closed, got {created}"
    );
    assert!(
        created["width"].as_u64().unwrap_or(0) > 0 && created["height"].as_u64().unwrap_or(0) > 0,
        "the create answers with the extent it actually minted, got {created}"
    );
    wait_until_the_pump_routes_to_exactly(1);

    // Naming a frame: reachable per-frame, answered without waiting for the
    // window, and never reporting a closed window that is not closed.
    let shown = helper.escalate_request_to_the_parent(show_surface_on_window(
        &published_surface_id.0,
        &window_id,
    ));
    assert_ok(&shown, "show_surface_on_processor_owned_window");
    assert_eq!(
        shown["processor_owned_window_is_closed"],
        json!(false),
        "an open window must not report itself closed, got {shown}"
    );

    // A described frame: the axis a Rust owner has had since the seam landed,
    // reachable from a helper process too. What the window server does with
    // BT.2020/PQ is its own call — what is asserted is that describing a
    // frame crosses the hop and never costs the owner its window.
    let shown_hdr = helper.escalate_request_to_the_parent(show_hdr_surface_on_window(
        &published_surface_id.0,
        &window_id,
    ));
    assert_ok(
        &shown_hdr,
        "show_surface_on_processor_owned_window carrying a colour description",
    );
    assert_eq!(
        shown_hdr["processor_owned_window_is_closed"],
        json!(false),
        "describing a frame must not close the window, got {shown_hdr}"
    );

    // The surface-id lifetime contract, over the wire: a retired id is a loud
    // recycled-frame error naming the recycling, never another frame.
    let retired = helper
        .escalate_request_to_the_parent(show_surface_on_window(&retired_frame_id, &window_id));
    let retired_message = refusal_message_of(&retired, "naming a retired frame id");
    assert!(
        retired_message.contains(&retired_frame_id),
        "the refusal must name the id that was retired, got: {retired_message}"
    );
    assert!(
        retired_message.contains("recycled"),
        "the refusal must say the frame was recycled rather than blaming the window, got: \
         {retired_message}"
    );

    // A window nobody owns is its own refusal, and not the different failure
    // of having no display server.
    let unowned = helper.escalate_request_to_the_parent(show_surface_on_window(
        &published_surface_id.0,
        A_WINDOW_ID_NOBODY_OWNS,
    ));
    let unowned_message = refusal_message_of(&unowned, "naming a window nobody owns");
    assert!(
        unowned_message.contains(A_WINDOW_ID_NOBODY_OWNS),
        "the refusal must name the window id, got: {unowned_message}"
    );

    // Polling: coalesced state, no callback across the hop.
    let drained = helper.escalate_request_to_the_parent(json!({
        "op": "drain_processor_owned_window_events",
        "window_id": window_id,
    }));
    assert_ok(&drained, "drain_processor_owned_window_events");
    assert!(
        drained["width"].as_u64().unwrap_or(0) > 0 && drained["height"].as_u64().unwrap_or(0) > 0,
        "the drain reports the window's current extent, got {drained}"
    );
    assert_eq!(
        drained["close_requested_by_user"],
        json!(false),
        "nobody closed this window, so no close-request may be reported, got {drained}"
    );
    assert_eq!(
        drained["processor_owned_window_is_closed"],
        json!(false),
        "an open window must not report itself closed, got {drained}"
    );

    // A second window, left for teardown to release.
    let second_created =
        helper.escalate_request_to_the_parent(create_window_titled("a window nobody closes"));
    assert_ok(&second_created, "a second create_processor_owned_window");
    wait_until_the_pump_routes_to_exactly(2);

    // Explicit release: the present thread joins and the window closes.
    let closed = helper.escalate_request_to_the_parent(json!({
        "op": "close_processor_owned_window",
        "window_id": window_id,
    }));
    assert_ok(&closed, "close_processor_owned_window");
    assert_eq!(
        closed["processor_owned_window_is_closed"],
        json!(true),
        "a closed window reports itself closed, got {closed}"
    );
    wait_until_the_pump_routes_to_exactly(1);

    // After a close, naming a frame is a no-op that reports the window
    // closed — never an error. The engine answers the same way whether the
    // owner closed the window or a user gesture did.
    let shown_after_close = helper.escalate_request_to_the_parent(show_surface_on_window(
        &published_surface_id.0,
        &window_id,
    ));
    assert_ok(
        &shown_after_close,
        "show_surface_on_processor_owned_window after a close",
    );
    assert_eq!(
        shown_after_close["processor_owned_window_is_closed"],
        json!(true),
        "naming a frame to a closed window must report it closed, got {shown_after_close}"
    );
    // Even with an id the window could never have shown: a closed window is
    // answered before the id is judged, so a stale id cannot turn the no-op
    // into the error the close was never allowed to be.
    let stale_id_after_close = helper
        .escalate_request_to_the_parent(show_surface_on_window(&retired_frame_id, &window_id));
    assert_ok(
        &stale_id_after_close,
        "naming a retired id to a closed window",
    );
    assert_eq!(
        stale_id_after_close["processor_owned_window_is_closed"],
        json!(true),
        "a closed window answers closed whatever id it was named, got {stale_id_after_close}"
    );

    // The setup-phase gate, engine-side: once the parent has moved the child
    // on, no window is minted however the child asks.
    send_lifecycle_command_and_let_the_helper_read_it(&bridge, &mut helper, "run");
    let refused_mid_pipeline =
        helper.escalate_request_to_the_parent(create_window_titled("a window asked for too late"));
    let refused_message = refusal_message_of(&refused_mid_pipeline, "minting a window from run");
    assert!(
        refused_message.contains("setup"),
        "the refusal must name the phase a window is asked for in, got: {refused_message}"
    );
    wait_until_the_pump_routes_to_exactly(1);

    // Teardown is the backstop: the window the owner never closed is released
    // with the processor, present thread joined and registration dropped.
    drop(bridge);
    wait_until_the_pump_routes_to_exactly(0);
}
