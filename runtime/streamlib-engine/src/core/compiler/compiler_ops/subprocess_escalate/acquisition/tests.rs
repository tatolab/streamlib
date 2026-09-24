// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[cfg(target_os = "linux")]
use super::linux::derive_texture_cross_process_importability;
use super::{parse_texture_format, parse_texture_usages, texture_usages_to_wire};
#[cfg(target_os = "linux")]
use crate::core::context::TextureCrossProcessImportability;
use crate::core::rhi::{PixelFormat, TextureFormat, TextureUsages};

#[test]
fn parse_pixel_format_accepts_common_aliases() {
    assert_eq!(
        PixelFormat::parse_wire_name("bgra"),
        Ok(PixelFormat::Bgra32)
    );
    assert_eq!(
        PixelFormat::parse_wire_name("BGRA32"),
        Ok(PixelFormat::Bgra32)
    );
    assert_eq!(
        PixelFormat::parse_wire_name("nv12"),
        Ok(PixelFormat::Nv12VideoRange)
    );
    assert_eq!(
        PixelFormat::parse_wire_name("nv12_full_range"),
        Ok(PixelFormat::Nv12FullRange)
    );
    assert_eq!(
        PixelFormat::parse_wire_name("gray8"),
        Ok(PixelFormat::Gray8)
    );
}

#[test]
fn parse_pixel_format_rejects_unknown() {
    assert!(PixelFormat::parse_wire_name("xyz").is_err());
}
#[test]
fn parse_texture_format_roundtrips_known_variants() {
    assert_eq!(
        parse_texture_format("bgra8_unorm"),
        Ok(TextureFormat::Bgra8Unorm)
    );
    assert_eq!(
        parse_texture_format("RGBA16_FLOAT"),
        Ok(TextureFormat::Rgba16Float)
    );
    assert_eq!(parse_texture_format("nv12"), Ok(TextureFormat::Nv12));
    assert!(parse_texture_format("xyz").is_err());
}

#[test]
fn parse_texture_usages_combines_tokens_and_implies_both_copy_bits() {
    let usage = parse_texture_usages(&["texture_binding".to_string()]).expect("known tokens");
    assert!(usage.contains(TextureUsages::TEXTURE_BINDING));
    assert!(
        usage.contains(TextureUsages::COPY_SRC | TextureUsages::COPY_DST),
        "one spelled token is enough: the CPU doors copy both ways, so a texture an author \
             acquired can always take them"
    );
    assert!(!usage.contains(TextureUsages::STORAGE_BINDING));

    let spelled_out = parse_texture_usages(&[
        "texture_binding".to_string(),
        "copy_src".to_string(),
        "copy_dst".to_string(),
    ])
    .expect("known tokens");
    assert_eq!(
        usage, spelled_out,
        "spelling the copy tokens must reach the same mask the implication does"
    );
}

#[test]
fn parse_texture_usages_rejects_empty_and_unknown() {
    assert!(
        parse_texture_usages(&[]).is_err(),
        "the implication rides a spelled usage; it never conjures a mask from nothing"
    );
    assert!(parse_texture_usages(&["bogus".to_string()]).is_err());
}

/// The implication reaches the caller's own echo: an author who asked
/// for one usage is told the three the texture actually carries, so the
/// door that later refuses on a missing copy bit is refusing about the
/// same mask the acquire reported.
#[test]
fn the_implied_copy_bits_echo_back_on_the_wire() {
    let usage = parse_texture_usages(&["texture_binding".to_string()]).expect("known tokens");
    assert_eq!(
        texture_usages_to_wire(usage),
        vec![
            "copy_src".to_string(),
            "copy_dst".to_string(),
            "texture_binding".to_string()
        ]
    );
}

/// Implying both copy bits must not move any request off the flavour it
/// takes today: the OPAQUE_FD fixed usage set already contains them, and
/// the render-attachment branch answers before usage is weighed.
#[cfg(target_os = "linux")]
#[test]
fn the_implied_copy_bits_leave_every_flavour_derivation_where_it_was() {
    let implied = parse_texture_usages(&["texture_binding".to_string()]).expect("known tokens");
    assert_eq!(
        derive_texture_cross_process_importability(TextureFormat::Rgba8Unorm, implied, true, true),
        TextureCrossProcessImportability::OpaqueFd,
    );

    let implied_render_attachment =
        parse_texture_usages(&["render_attachment".to_string()]).expect("known tokens");
    assert_eq!(
        derive_texture_cross_process_importability(
            TextureFormat::Rgba8Unorm,
            implied_render_attachment,
            true,
            true,
        ),
        TextureCrossProcessImportability::RenderTargetDmaBuf,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_render_attachment_request_takes_the_modifier_flavor_when_the_probe_has_one() {
    let usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    assert_eq!(
        derive_texture_cross_process_importability(TextureFormat::Rgba8Unorm, usage, true, true),
        TextureCrossProcessImportability::RenderTargetDmaBuf,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_render_attachment_request_without_a_modifier_stays_not_importable() {
    let usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    assert_eq!(
        derive_texture_cross_process_importability(TextureFormat::Rgba8Unorm, usage, false, true),
        TextureCrossProcessImportability::NotImportable,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_multi_plane_format_never_takes_the_modifier_flavor() {
    let usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    assert_eq!(
        derive_texture_cross_process_importability(TextureFormat::Nv12, usage, true, true),
        TextureCrossProcessImportability::NotImportable,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_device_without_the_opaque_fd_pool_falls_back_to_not_importable() {
    let usage = TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING;
    assert_eq!(
        derive_texture_cross_process_importability(TextureFormat::Rgba8Unorm, usage, false, false),
        TextureCrossProcessImportability::NotImportable,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_cuda_mappable_format_within_the_fixed_usage_set_takes_opaque_fd() {
    let usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::STORAGE_BINDING
        | TextureUsages::COPY_SRC
        | TextureUsages::COPY_DST;
    for format in [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
    ] {
        assert_eq!(
            derive_texture_cross_process_importability(format, usage, false, true),
            TextureCrossProcessImportability::OpaqueFd,
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_format_cuda_cannot_map_stays_not_importable_without_render_attachment() {
    for format in [
        TextureFormat::Bgra8Unorm,
        TextureFormat::Bgra8UnormSrgb,
        TextureFormat::Rgba8UnormSrgb,
        TextureFormat::Nv12,
    ] {
        assert_eq!(
            derive_texture_cross_process_importability(
                format,
                TextureUsages::TEXTURE_BINDING,
                true,
                true,
            ),
            TextureCrossProcessImportability::NotImportable,
        );
    }
}

#[test]
fn texture_usages_to_wire_is_stable_order() {
    let usage =
        TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING;
    assert_eq!(
        texture_usages_to_wire(usage),
        vec![
            "copy_src".to_string(),
            "texture_binding".to_string(),
            "storage_binding".to_string()
        ]
    );
}
