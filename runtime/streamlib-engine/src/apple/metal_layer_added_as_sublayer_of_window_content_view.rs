// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `CAMetalLayer` a window presents through, added as a sublayer of the
//! window's content view on the process's first thread.
//!
//! AppKit lets only the first thread touch a view, and winit hands out a raw
//! window handle only there, while a present target is minted on its owner's
//! render thread. So the layer is added here, when the window is minted, and
//! the present target is minted from the layer alone.

use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_quartz_core::{CAAutoresizingMask, CAMetalLayer};
use winit::window::Window;

use super::appkit_content_view_of_winit_window::appkit_content_view_of;
use crate::core::{Error, Result};

/// A `CAMetalLayer` sized to a window's content view, for its present target.
pub struct MetalLayerAddedAsSublayerOfWindowContentView {
    metal_layer: Retained<CAMetalLayer>,
}

// SAFETY: this type sends the layer no message after
// `add_as_a_sublayer_of_the_content_view_of`, which runs on the first thread;
// it only hands out the pointer, and MoltenVK's own thread-safety contract
// covers what it sends through the surface minted from it. Retain and release
// of an Objective-C object are thread-safe.
unsafe impl Send for MetalLayerAddedAsSublayerOfWindowContentView {}

impl MetalLayerAddedAsSublayerOfWindowContentView {
    /// Add a fresh `CAMetalLayer` over `window`'s content view, tracking its
    /// size and matching its backing scale.
    pub fn add_as_a_sublayer_of_the_content_view_of(
        window: &Window,
        first_thread: MainThreadMarker,
    ) -> Result<Self> {
        let content_view = appkit_content_view_of(window, first_thread)?;

        // winit makes its content view layer-backed, so AppKit owns the root
        // layer and resizes it with the view; the Metal layer rides it as a
        // sublayer rather than replacing it.
        content_view.setWantsLayer(true);
        let content_view_root_layer = content_view.layer().ok_or_else(|| {
            Error::DisplaySurfaceUnavailable(
                "the window's content view has no backing layer to add a Metal layer to".into(),
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
