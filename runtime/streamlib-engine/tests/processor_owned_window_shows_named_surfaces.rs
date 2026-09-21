// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A processor-owned window driven directly rather than through a processor:
//! name a published surface and it reaches the window; name one that resolves
//! to nothing and the window keeps what it already has.
//!
//! Its own process, because opening one mints the process's one event loop
//! and a Vulkan device alongside it. One test function for the same reason —
//! a second `GpuDevice::new()` in this binary would build a second device.
//!
//! Display tier: needs a display server and a GPU, so it runs on the rig
//! only. The end-to-end evidence that N of these coexist is
//! `streamlib-media-builtins`' `two_display_windows_live`.

#![cfg(target_os = "linux")]

mod processor_owned_window_named_surface_contract;

use streamlib_engine::core::context::GpuContext;
use streamlib_engine::core::processor_owned_window::ProcessorOwnedWindowAwaitingItsPresentTarget;

use processor_owned_window_named_surface_contract::{
    mint_and_hold_a_window_to_the_named_surface_contract, request_for,
};

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "display tier — needs a display server ($DISPLAY / $WAYLAND_DISPLAY) + a GPU. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
#[test]
fn a_named_surface_reaches_the_window_and_an_unresolvable_id_leaves_the_last_frame_up() {
    let gpu_context = GpuContext::init_for_platform().expect("a GPU is required for this tier");
    let registered_window =
        ProcessorOwnedWindowAwaitingItsPresentTarget::register_on_the_process_wide_window_event_pump(
            request_for("streamlib processor-owned window test"),
        )
        .expect("the pump mints a window");

    let (processor_owned_window, source_texture) =
        mint_and_hold_a_window_to_the_named_surface_contract(
            &gpu_context.limited_access(),
            registered_window,
        );

    drop(processor_owned_window);
    drop(source_texture);
}
