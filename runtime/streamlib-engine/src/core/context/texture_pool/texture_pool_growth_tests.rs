// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::{TextureFormat, TextureUsages};

/// The runtime-wide pool caps no shape by count: with every texture of a
/// shape held, the next acquire allocates another.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[allow(clippy::disallowed_macros)]
fn the_texture_pool_holds_more_than_sixteen_textures_of_one_shape_at_once() {
    let Ok(device) = GpuDevice::new() else {
        println!("no GPU device — skipping");
        return;
    };
    let pool = TexturePool::new(Arc::new(device));
    let desc = TexturePoolDescriptor::new(16, 16, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST);
    let held: Vec<PooledTextureHandle> = (0..17)
        .map(|_| {
            pool.acquire(&desc, None)
                .expect("an acquire with every texture held allocates")
        })
        .collect();
    let distinct_slot_ids: std::collections::HashSet<PoolSlotId> =
        held.iter().map(PooledTextureHandle::slot_id).collect();
    assert_eq!(
        distinct_slot_ids.len(),
        held.len(),
        "every held texture is its own slot"
    );
    assert_eq!(pool.stats().textures_in_use, held.len());
}
