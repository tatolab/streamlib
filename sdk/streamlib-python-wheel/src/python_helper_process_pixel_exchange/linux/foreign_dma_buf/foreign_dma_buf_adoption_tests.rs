// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

const ADOPTED_WIDTH: u32 = 64;
const ADOPTED_HEIGHT: u32 = 16;
const ADOPTED_BYTE_SIZE: u64 = 64 * 4 * 16;

/// A sized memfd standing in for a foreign DMA-BUF.
fn a_foreign_memory_fd() -> OwnedFd {
    let raw_fd =
        unsafe { libc::memfd_create(c"adopted-foreign-plane".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw_fd >= 0, "memfd_create failed");
    // SAFETY: a fresh fd memfd_create just returned; ours alone.
    let owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let sized = unsafe { libc::ftruncate(owned.as_raw_fd(), ADOPTED_BYTE_SIZE as i64) };
    assert_eq!(sized, 0, "ftruncate failed");
    owned
}

fn check_in_request() -> serde_json::Value {
    serde_json::json!({
        "op": "check_in",
        "runtime_id": "helper:adoption-under-test",
        "width": ADOPTED_WIDTH,
        "height": ADOPTED_HEIGHT,
        "format": "bgra32",
        "resource_type": "pixel_buffer",
        "handle_type": "dma_buf",
        "plane_sizes": [ADOPTED_BYTE_SIZE],
        "plane_offsets": [0u64],
        "plane_strides": [ADOPTED_BYTE_SIZE / u64::from(ADOPTED_HEIGHT)],
    })
}

/// The fd crosses, the service mints a resolvable id, and the checkout
/// hands the plane back with the geometry the adoption declared.
#[test]
fn an_adopted_foreign_fd_is_resolvable_with_its_declared_geometry() {
    let share = SurfaceShareUnderTest::start("adoption");
    let exchange_client = share.exchange_client_labelled("adoption-under-test");
    let foreign_fd = a_foreign_memory_fd();

    let (check_in_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(&check_in_request(), &[foreign_fd.as_raw_fd()])
        .expect("the check_in round trip completes");
    let adopted_surface_id = check_in_response
        .get("surface_id")
        .and_then(|value| value.as_str())
        .expect("check_in answers with a surface_id")
        .to_string();
    // The original stays the caller's — the kernel and the service each
    // dup'd it on the way over.
    drop(foreign_fd);

    let (check_out_response, received_fds) = exchange_client
        .check_out_surface(&adopted_surface_id)
        .expect("the adopted surface checks out");
    assert!(
        check_out_response.get("error").is_none(),
        "the checkout must not refuse: {check_out_response}"
    );
    assert_eq!(
        check_out_response.get("width").and_then(|v| v.as_u64()),
        Some(u64::from(ADOPTED_WIDTH)),
    );
    assert_eq!(
        check_out_response.get("height").and_then(|v| v.as_u64()),
        Some(u64::from(ADOPTED_HEIGHT)),
    );
    assert_eq!(
        check_out_response.get("format").and_then(|v| v.as_str()),
        Some("bgra32"),
    );
    assert_eq!(
        check_out_response
            .get("plane_sizes")
            .and_then(|v| v.as_array())
            .map(|sizes| sizes.iter().filter_map(|s| s.as_u64()).collect::<Vec<_>>()),
        Some(vec![ADOPTED_BYTE_SIZE]),
    );
    assert_eq!(
        received_fds.len(),
        1,
        "one adopted plane crosses back as one fd"
    );
    drop(received_fds);
    let _ = exchange_client.release_check_out(&adopted_surface_id);
}

/// The unregister debt's op removes the registration: the id stops
/// resolving, and a second release reports nothing left to remove.
#[test]
fn releasing_an_adoption_removes_the_registration() {
    let share = SurfaceShareUnderTest::start("adoption-release");
    let exchange_client = share.exchange_client_labelled("adoption-under-test");
    let foreign_fd = a_foreign_memory_fd();

    let (check_in_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(&check_in_request(), &[foreign_fd.as_raw_fd()])
        .expect("the check_in round trip completes");
    let adopted_surface_id = check_in_response
        .get("surface_id")
        .and_then(|value| value.as_str())
        .expect("check_in answers with a surface_id")
        .to_string();

    exchange_client
        .unregister_foreign_surface(&adopted_surface_id)
        .expect("the adoption unregisters");

    let (after_release, _no_fds_after) = exchange_client
        .check_out_surface(&adopted_surface_id)
        .expect("the socket round trip still completes");
    assert!(
        after_release.get("error").is_some(),
        "a released adoption must stop resolving: {after_release}"
    );
    let second_release = exchange_client.unregister_foreign_surface(&adopted_surface_id);
    assert!(
        second_release.is_err(),
        "a second release must report nothing left to remove"
    );
}

/// Register `plane_count` memfd planes under `handle_type` with the extra
/// registration fields `extra_fields`, and return the minted surface id.
///
/// No Vulkan runs on these paths: every refusal below fires before the
/// helper creates its import device, so they hold on a box with no GPU.
fn a_pixel_registration_of(
    exchange_client: &HelperProcessGpuExchangeClient,
    handle_type: &str,
    plane_count: usize,
    extra_fields: serde_json::Value,
) -> String {
    let backing_fds: Vec<OwnedFd> = (0..plane_count).map(|_| a_foreign_memory_fd()).collect();
    let mut check_in = check_in_request();
    check_in["handle_type"] = handle_type.into();
    check_in["plane_sizes"] = vec![ADOPTED_BYTE_SIZE; plane_count].into();
    check_in["plane_offsets"] = vec![0u64; plane_count].into();
    check_in["plane_strides"] =
        vec![ADOPTED_BYTE_SIZE / u64::from(ADOPTED_HEIGHT); plane_count].into();
    for (field, value) in extra_fields.as_object().into_iter().flatten() {
        check_in[field] = value.clone();
    }
    let raw_backing_fds: Vec<_> = backing_fds.iter().map(|fd| fd.as_raw_fd()).collect();
    let (check_in_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(&check_in, &raw_backing_fds)
        .expect("the check_in round trip completes");
    check_in_response
        .get("surface_id")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("check_in answers with a surface_id: {check_in_response}"))
        .to_string()
}

/// Every pooled frame is one RGBA allocation, so a multi-plane OPAQUE_FD
/// pixel buffer has no producer; it is refused naming the surface and the
/// plane count rather than imported as its first plane.
#[test]
fn a_multi_plane_opaque_fd_pixel_buffer_is_refused_by_name() {
    let share = SurfaceShareUnderTest::start("opaque-pixel-multi-plane");
    let exchange_client = share.exchange_client_labelled("adoption-under-test");
    let surface_id = a_pixel_registration_of(
        &exchange_client,
        "opaque_fd",
        2,
        serde_json::json!({ "vk_memory_type_index": 3u32 }),
    );

    let refusal = exchange_client
        .check_out_and_import(&surface_id)
        .err()
        .expect("a two-plane opaque_fd pixel buffer must be refused")
        .to_string();
    assert!(
        refusal.contains(&format!("{surface_id:?}")),
        "the refusal must name the surface: {refusal:?}"
    );
    assert!(
        refusal.contains("opaque_fd") && refusal.contains("2 planes"),
        "the refusal must name the flavour and the plane count: {refusal:?}"
    );
}

/// A single-plane OPAQUE_FD pixel buffer imports at the memory type index
/// its exporter allocated from; a registration that does not state one is
/// refused naming the field rather than imported at a guessed index.
#[test]
fn an_opaque_fd_pixel_buffer_without_a_memory_type_index_is_refused_by_name() {
    let share = SurfaceShareUnderTest::start("opaque-pixel-no-index");
    let exchange_client = share.exchange_client_labelled("adoption-under-test");
    let surface_id =
        a_pixel_registration_of(&exchange_client, "opaque_fd", 1, serde_json::json!({}));

    let refusal = exchange_client
        .check_out_and_import(&surface_id)
        .err()
        .expect("an opaque_fd pixel buffer without its memory type index must be refused")
        .to_string();
    assert!(
        refusal.contains(&format!("{surface_id:?}")),
        "the refusal must name the surface: {refusal:?}"
    );
    assert!(
        refusal.contains("vk_memory_type_index"),
        "the refusal must name the missing field: {refusal:?}"
    );
}

#[test]
fn a_pixel_buffer_of_an_unknown_handle_type_is_refused_by_name() {
    let share = SurfaceShareUnderTest::start("unknown-pixel-handle");
    let exchange_client = share.exchange_client_labelled("adoption-under-test");
    let surface_id = a_pixel_registration_of(
        &exchange_client,
        "win32_kmt",
        1,
        serde_json::json!({ "vk_memory_type_index": 3u32 }),
    );

    let refusal = exchange_client
        .check_out_and_import(&surface_id)
        .err()
        .expect("a pixel buffer of a handle type this consumer does not know must be refused")
        .to_string();
    assert!(
        refusal.contains(&format!("{surface_id:?}")) && refusal.contains("\"win32_kmt\""),
        "the refusal must name the surface and the handle type: {refusal:?}"
    );
}

/// A V4L2-shaped NV12 frame: 64-byte rows padded to 128, chroma starting
/// past an aligned luma plane, the allocation ending on chroma's last row.
fn a_padded_nv12_layout() -> (u64, PixelPlanesWithinOneAllocation) {
    let chroma_offset = 128 * u64::from(ADOPTED_HEIGHT) + 256;
    let allocation_byte_size = chroma_offset + 128 * (u64::from(ADOPTED_HEIGHT) / 2 - 1) + 64;
    foreign_dma_buf_plane_layout(
        PixelFormat::Nv12FullRange,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        Some(allocation_byte_size),
        Some(vec![0, chroma_offset]),
        Some(vec![128, 128]),
        0,
    )
    .expect("the padded NV12 layout is accepted")
}

/// The stated plane layout and the foreign-writer mark ride the check_in and
/// come back on the checkout, so the parent's converter reads the frame
/// through the pitch and offsets its exporter chose.
#[test]
fn an_adopted_padded_nv12_frame_checks_out_with_its_plane_layout() {
    let share = SurfaceShareUnderTest::start("adoption-nv12-layout");
    let exchange_client = share.exchange_client_labelled("adoption-under-test");
    let (allocation_byte_size, planes) = a_padded_nv12_layout();
    let foreign_fd = a_foreign_memory_fd();

    let mut check_in = check_in_request();
    check_in["format"] = PixelFormat::Nv12FullRange.wire_name().into();
    check_in["plane_sizes"] = vec![allocation_byte_size].into();
    check_in["plane_strides"] = vec![128u64].into();
    planes.write_surface_share_fields(&mut check_in);
    let (check_in_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(&check_in, &[foreign_fd.as_raw_fd()])
        .expect("the check_in round trip completes");
    let adopted_surface_id = check_in_response
        .get("surface_id")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("check_in answers with a surface_id: {check_in_response}"))
        .to_string();

    let (check_out_response, received_fds) = exchange_client
        .check_out_surface(&adopted_surface_id)
        .expect("the adopted surface checks out");
    assert_eq!(
        PixelPlanesWithinOneAllocation::from_surface_share_fields(&check_out_response)
            .expect("the echoed layout parses"),
        Some(planes),
    );
    drop(received_fds);
    let _ = exchange_client.release_check_out(&adopted_surface_id);
}

#[test]
fn an_unstated_single_plane_layout_spreads_the_byte_size_over_whole_rows() {
    let (allocation_byte_size, planes) = foreign_dma_buf_plane_layout(
        PixelFormat::Bgra32,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        Some(ADOPTED_BYTE_SIZE * 2),
        None,
        None,
        0,
    )
    .expect("a whole number of padded rows is accepted");
    assert_eq!(allocation_byte_size, ADOPTED_BYTE_SIZE * 2);
    assert_eq!(
        planes.plane_strides_bytes(),
        &[2 * 4 * u64::from(ADOPTED_WIDTH)]
    );
    assert!(planes.written_by_a_foreign_device());
}

#[test]
fn an_unstated_nv12_layout_is_tightly_packed() {
    let (allocation_byte_size, planes) = foreign_dma_buf_plane_layout(
        PixelFormat::Nv12VideoRange,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        None,
        None,
        None,
        0,
    )
    .expect("tight NV12 is accepted");
    let luma_byte_size = u64::from(ADOPTED_WIDTH * ADOPTED_HEIGHT);
    assert_eq!(allocation_byte_size, luma_byte_size * 3 / 2);
    assert_eq!(planes.plane_offsets_bytes(), &[0, luma_byte_size]);
}

#[test]
fn a_stated_layout_without_its_byte_size_is_refused() {
    let refusal = foreign_dma_buf_plane_layout(
        PixelFormat::Nv12FullRange,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        None,
        Some(vec![0, 4096]),
        Some(vec![128, 128]),
        0,
    )
    .expect_err("a layout without the allocation it sits in must be refused");
    assert!(refusal.contains("byte_size"), "{refusal:?}");
}

#[test]
fn offsets_without_strides_are_refused() {
    let refusal = foreign_dma_buf_plane_layout(
        PixelFormat::Nv12FullRange,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        Some(ADOPTED_BYTE_SIZE),
        Some(vec![0, 1024]),
        None,
        0,
    )
    .expect_err("half a layout must be refused");
    assert!(refusal.contains("together"), "{refusal:?}");
}

#[test]
fn a_tiled_modifier_is_refused_by_value() {
    let refusal = foreign_dma_buf_plane_layout(
        PixelFormat::Bgra32,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        None,
        None,
        None,
        0x0100_0000_0000_0001,
    )
    .expect_err("a tiled buffer must be refused");
    assert!(refusal.contains("0x100000000000001"), "{refusal:?}");
}

/// NVIDIA's block-linear layout, the modifier a page CEF paints on an NVIDIA
/// GPU arrives with.
const NVIDIA_BLOCK_LINEAR_DRM_FORMAT_MODIFIER: u64 = 0x0300_0000_0060_6014;

fn a_tiled_bgra_texture() -> ForeignDmaBufTextureWithATiledModifier {
    ForeignDmaBufTextureWithATiledModifier::described_by(
        PixelFormat::Bgra32,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        ADOPTED_BYTE_SIZE,
        &[0],
        &[ADOPTED_WIDTH as u64 * 4],
        NVIDIA_BLOCK_LINEAR_DRM_FORMAT_MODIFIER,
    )
    .expect("one bgra32 plane with a tiled modifier is a texture")
}

/// A tiled BGRA paint registers as a texture the parent imports through its
/// modifier: the lookup answers the texture vocabulary, the one plane's
/// layout, and the read-only usage the parent imports it with.
#[test]
fn a_tiled_bgra_dma_buf_registers_as_a_texture_with_its_modifier() {
    let share = SurfaceShareUnderTest::start("adoption-tiled-texture");
    let exchange_client = share.exchange_client_labelled("adoption-under-test");
    let foreign_fd = a_foreign_memory_fd();

    let adopted_surface_id = exchange_client
        .register_foreign_dma_buf_texture_with_a_tiled_modifier(
            foreign_fd.as_raw_fd(),
            &a_tiled_bgra_texture(),
        )
        .expect("the service adopts the tiled texture");
    drop(foreign_fd);

    let (lookup_response, received_fds) = exchange_client
        .surface_share_request_with_fds(
            &serde_json::json!({"op": "lookup", "surface_id": adopted_surface_id}),
            &[],
        )
        .expect("the lookup round trip completes");
    drop(received_fds);
    assert!(
        lookup_response.get("error").is_none(),
        "the lookup must not refuse: {lookup_response}"
    );
    assert_eq!(lookup_response["resource_type"], "texture");
    assert_eq!(lookup_response["format"], "bgra8_unorm");
    assert_eq!(
        lookup_response["drm_format_modifier"],
        NVIDIA_BLOCK_LINEAR_DRM_FORMAT_MODIFIER
    );
    assert_eq!(lookup_response["plane_offsets"], serde_json::json!([0]));
    assert_eq!(
        lookup_response["plane_strides"],
        serde_json::json!([ADOPTED_WIDTH * 4])
    );
    assert_eq!(
        lookup_response["vk_image_allocation_size"],
        ADOPTED_BYTE_SIZE
    );
    assert_eq!(lookup_response["vk_image_usage"], 0x05);

    exchange_client
        .unregister_foreign_surface(&adopted_surface_id)
        .expect("the adoption unregisters");
}

#[test]
fn a_tiled_rgba_dma_buf_is_an_rgba8_texture() {
    let texture = ForeignDmaBufTextureWithATiledModifier::described_by(
        PixelFormat::Rgba32,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        ADOPTED_BYTE_SIZE,
        &[0],
        &[256],
        NVIDIA_BLOCK_LINEAR_DRM_FORMAT_MODIFIER,
    )
    .expect("one rgba32 plane with a tiled modifier is a texture");
    assert_eq!(texture.texture_format, TextureFormat::Rgba8Unorm);
}

#[test]
fn a_tiled_ycbcr_dma_buf_is_refused_naming_its_format() {
    let refusal = ForeignDmaBufTextureWithATiledModifier::described_by(
        PixelFormat::Nv12FullRange,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        ADOPTED_BYTE_SIZE,
        &[0, 1024],
        &[64, 64],
        NVIDIA_BLOCK_LINEAR_DRM_FORMAT_MODIFIER,
    )
    .expect_err("a tiled YCbCr buffer is no texture the converter samples");
    assert!(refusal.contains("nv12"), "{refusal:?}");
}

#[test]
fn a_tiled_dma_buf_with_two_planes_is_refused() {
    let refusal = ForeignDmaBufTextureWithATiledModifier::described_by(
        PixelFormat::Bgra32,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        ADOPTED_BYTE_SIZE,
        &[0, 2048],
        &[256, 256],
        NVIDIA_BLOCK_LINEAR_DRM_FORMAT_MODIFIER,
    )
    .expect_err("a tiled image binds one plane");
    assert!(refusal.contains("one plane"), "{refusal:?}");
}

#[test]
fn a_tiled_plane_outside_its_allocation_is_refused() {
    let refusal = ForeignDmaBufTextureWithATiledModifier::described_by(
        PixelFormat::Bgra32,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        ADOPTED_BYTE_SIZE,
        &[ADOPTED_BYTE_SIZE],
        &[256],
        NVIDIA_BLOCK_LINEAR_DRM_FORMAT_MODIFIER,
    )
    .expect_err("the plane must start inside the allocation");
    assert!(refusal.contains("outside"), "{refusal:?}");
}

#[test]
fn a_linear_dma_buf_is_not_a_tiled_texture() {
    let refusal = ForeignDmaBufTextureWithATiledModifier::described_by(
        PixelFormat::Bgra32,
        ADOPTED_WIDTH,
        ADOPTED_HEIGHT,
        ADOPTED_BYTE_SIZE,
        &[0],
        &[256],
        DRM_FORMAT_MOD_LINEAR,
    )
    .expect_err("a linear buffer reads through its plane layout");
    assert!(refusal.contains("linear"), "{refusal:?}");
}
