// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Publishing to the surface-share service the layout a dispatch left each texture it bound in.

use crate::core::context::{GpuContext, SurfaceStore, TextureRegistration};
use crate::core::rhi::VulkanLayout;

/// Publish to the surface-share service the layout each of `bound_textures` was left in by a
/// dispatch this process ran, so a reader in another process barriers out of the layout the
/// image is in.
pub(crate) fn publish_the_layouts_a_dispatch_left_its_bound_textures_in(
    gpu: &GpuContext,
    bound_textures: &[(String, TextureRegistration)],
) {
    if bound_textures.is_empty() {
        return;
    }
    publish_the_layouts_these_textures_rest_in(gpu.surface_store(), bound_textures);
}

/// Publish each of `bound_textures`' layouts to `surface_store`, a surface several entries name
/// once, in the layout of its last entry, and answer each surface with that layout.
///
/// Best-effort: an id the service does not hold is an in-process-only surface, not an error, and
/// a publish failure costs the reader its content-preserving acquire, never the dispatch.
pub(crate) fn publish_the_layouts_these_textures_rest_in(
    surface_store: Option<SurfaceStore>,
    bound_textures: &[(String, TextureRegistration)],
) -> Vec<(String, VulkanLayout)> {
    let mut layouts_published: Vec<(String, VulkanLayout)> =
        Vec::with_capacity(bound_textures.len());
    // Walked back to front: a surface several dispatches of one batch bind ends in the layout its
    // last use required, and a cross-process resolve holds a separate layout cell per occurrence.
    for (surface_id, registration) in bound_textures.iter().rev() {
        if layouts_published
            .iter()
            .any(|(already_published, _)| already_published == surface_id)
        {
            continue;
        }
        let image_layout = registration.current_layout();
        if let Some(store) = &surface_store
            && let Err(publish_failure) = store.update_image_layout(surface_id, image_layout)
        {
            tracing::debug!(
                surface_id,
                %publish_failure,
                "the layout a dispatch left a texture in was not published"
            );
        }
        layouts_published.push((surface_id.clone(), image_layout));
    }
    layouts_published
}
