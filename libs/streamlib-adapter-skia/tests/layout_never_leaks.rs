// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `layout_never_leaks` — internal-only assertion that the Skia
//! adapter's public API surface never returns a `GrVkImageInfo` (or
//! any Vulkan handle / layout) to customers. Only `skia::Surface`
//! (write) and `skia::Image` (read) are reachable through the
//! `SurfaceAdapter` trait.
//!
//! The Vulkan layout escape hatch from `streamlib-adapter-abi`
//! (`VulkanWritable::vk_image_layout()`, `VulkanImageInfoExt::vk_image_info()`)
//! is what `SkiaSurfaceAdapter` uses internally to construct
//! Skia's `GrVkImageInfo`. Customers of `SurfaceAdapter` must NOT be
//! able to reach those — Skia's history of `GrVkImageInfo.fImageLayout`
//! leaking is exactly the failure mode we're avoiding here.
//!
//! Implementation: this is a compile-time check via the
//! `assert_not_impl_all!`-style ambiguous-impl trick — same pattern
//! the Vulkan adapter uses to assert its views don't impl
//! `CpuReadable`. If `SkiaWriteView` ever starts impl'ing
//! `VulkanWritable` or `VulkanImageInfoExt`, this test stops
//! compiling.

#![cfg(target_os = "linux")]

use streamlib_adapter_abi::{VulkanImageInfoExt, VulkanWritable};
use streamlib_adapter_skia::{SkiaReadView, SkiaWriteView};
use streamlib_consumer_rhi::ConsumerVulkanDevice;

trait AmbiguousIfImpl<A> {
    #[allow(dead_code)]
    fn some_item() {}
}
impl<T: ?Sized> AmbiguousIfImpl<()> for T {}

#[allow(dead_code)]
struct ImplsVulkanWritable;
impl<T: ?Sized + VulkanWritable> AmbiguousIfImpl<ImplsVulkanWritable> for T {}

#[allow(dead_code)]
struct ImplsVulkanImageInfoExt;
impl<T: ?Sized + VulkanImageInfoExt> AmbiguousIfImpl<ImplsVulkanImageInfoExt> for T {}

const _: fn() = || {
    // If SkiaWriteView<'static, ConsumerVulkanDevice> impls
    // VulkanWritable, the call below is ambiguous and the const-fn
    // body fails to compile. Same for SkiaReadView. The negative
    // assertion holds today: both views expose only Skia handles.
    let _ = <SkiaWriteView<'static, ConsumerVulkanDevice> as AmbiguousIfImpl<_>>::some_item;
    let _ = <SkiaReadView<'static, ConsumerVulkanDevice> as AmbiguousIfImpl<_>>::some_item;
};

#[test]
fn skia_views_do_not_expose_vulkan_image_info() {
    // The compile-time assertion above is the actual gate; this
    // function exists so `cargo test -p streamlib-adapter-skia
    // --test layout_never_leaks` reports a green status when the
    // crate compiles.
    println!(
        "SkiaWriteView and SkiaReadView do not implement VulkanWritable / VulkanImageInfoExt — \
         layout state stays inside the adapter."
    );
}
