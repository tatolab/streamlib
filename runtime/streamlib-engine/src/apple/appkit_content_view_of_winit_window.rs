// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The AppKit content view behind a winit window, reachable only on the
//! process's first thread, and what the engine sets on its window there.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSView, NSWindowAnimationBehavior};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::core::{Error, Result};

/// The content view of `window`, borrowed for as long as the window is.
pub fn appkit_content_view_of(
    window: &Window,
    _only_on_the_first_thread: MainThreadMarker,
) -> Result<&NSView> {
    let raw_window_handle = window
        .window_handle()
        .map_err(|e| {
            Error::DisplaySurfaceUnavailable(format!(
                "the window's AppKit handle is unavailable: {e}"
            ))
        })?
        .as_raw();
    let RawWindowHandle::AppKit(appkit_window_handle) = raw_window_handle else {
        return Err(Error::DisplaySurfaceUnavailable(format!(
            "expected an AppKit window handle, got {raw_window_handle:?}"
        )));
    };
    // SAFETY: winit's handle names the live content view of `window`, which the
    // returned borrow cannot outlive, and this is the first thread.
    Ok(unsafe { appkit_window_handle.ns_view.cast::<NSView>().as_ref() })
}

/// Make `window` leave the screen as soon as it closes rather than animate
/// out. AppKit runs the close animation on the app's run loop, and a window
/// closed as the event loop exits would otherwise stay on screen until the
/// loop runs long enough to finish it — after `rt.run()` returns, never.
pub fn close_without_animating(window: &Window, first_thread: MainThreadMarker) -> Result<()> {
    let ns_window = appkit_content_view_of(window, first_thread)?
        .window()
        .ok_or_else(|| {
            Error::DisplaySurfaceUnavailable("the content view is not in a window".into())
        })?;
    ns_window.setAnimationBehavior(NSWindowAnimationBehavior::None);
    Ok(())
}
