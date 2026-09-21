// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `CAMetalLayer` a window presents through, attached to the window's
//! content view on the process's first thread.
//!
//! AppKit lets only the first thread touch a view, and winit hands out a raw
//! window handle only there, while a present target is minted on its owner's
//! render thread. So the layer is attached here, when the window is minted, and
//! the present target is minted from the layer alone.

use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_quartz_core::{CAAutoresizingMask, CAMetalLayer};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::core::{Error, Result};

/// A `CAMetalLayer` sized to a window's content view, for its present target.
pub struct MetalLayerBackingWindowContentView {
    metal_layer: Retained<CAMetalLayer>,
}

// SAFETY: every message that mutates the layer or its view is sent in
// `attach_to_the_content_view_of`, on the first thread. Afterwards the value is
// only read for its pointer, which `vkCreateMetalSurfaceEXT` accepts from any
// thread, and retain/release of an Objective-C object is thread-safe.
unsafe impl Send for MetalLayerBackingWindowContentView {}
// SAFETY: as for `Send` — no `&self` method sends the layer a message.
unsafe impl Sync for MetalLayerBackingWindowContentView {}

impl MetalLayerBackingWindowContentView {
    /// Attach a fresh `CAMetalLayer` over `window`'s content view, tracking its
    /// size and matching its backing scale.
    pub fn attach_to_the_content_view_of(
        window: &Window,
        _only_on_the_first_thread: MainThreadMarker,
    ) -> Result<Self> {
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
        // SAFETY: winit's handle names the live content view of `window`, which
        // the caller keeps alive for this call, and this is the first thread.
        let content_view = unsafe { appkit_window_handle.ns_view.cast::<NSView>().as_ref() };

        // winit makes its content view layer-backed, so AppKit owns the root
        // layer and resizes it with the view; the Metal layer rides it as a
        // sublayer rather than replacing it.
        content_view.setWantsLayer(true);
        let content_view_root_layer = content_view.layer().ok_or_else(|| {
            Error::DisplaySurfaceUnavailable(
                "the window's content view has no backing layer to attach a Metal layer to".into(),
            )
        })?;

        let metal_layer = CAMetalLayer::new();
        metal_layer.setFrame(content_view_root_layer.bounds());
        metal_layer.setAutoresizingMask(
            CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable,
        );
        if let Some(ns_window) = content_view.window() {
            metal_layer.setContentsScale(ns_window.backingScaleFactor());
        }
        content_view_root_layer.addSublayer(&metal_layer);

        Ok(Self { metal_layer })
    }

    /// The layer, as the `CAMetalLayer*` `vkCreateMetalSurfaceEXT` takes.
    pub fn metal_layer_pointer(&self) -> *const c_void {
        Retained::as_ptr(&self.metal_layer).cast()
    }
}
