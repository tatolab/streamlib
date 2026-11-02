// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A texture's surface-share `check_out` answer, read into typed fields: its
//! extent and format, the flavour its tiled memory crossed as, the layout the
//! last holder published, and its timeline edges. Also the `update_layout`
//! publish a producer makes after its release barrier.

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;

use crate::linux::{MAX_SCM_RIGHTS_FDS, send_request_with_fds};
use crate::linux_pixel_buffer_check_out::{
    SurfaceShareCheckOutFds, device_uuid_of_its_hex_spelling,
    refusal_the_surface_share_service_answered, resource_type_of_a_check_out,
};

/// The `vk_image_usage` a registration that states none was created with:
/// transfer source and destination, sampled and storage.
const VK_IMAGE_USAGE_OF_A_REGISTRATION_THAT_STATES_NONE: u32 = 0x0F;

/// The flavour a checked-out texture's single memory fd crossed as.
pub enum CheckedOutTextureMemory {
    /// A DMA-BUF holding a tiled image under an explicit DRM format modifier.
    DmaBufWithExplicitDrmFormatModifier {
        memory_fd: OwnedFd,
        plane_offsets: Vec<u64>,
        plane_strides: Vec<u64>,
        drm_format_modifier: u64,
    },
    /// One OPAQUE_FD allocation, rebuilt with the exporter's image usage and
    /// bound at the memory type index it allocated from.
    OpaqueFd {
        memory_fd: OwnedFd,
        vk_image_usage_flags: u32,
        vk_memory_type_index: u32,
        exporting_device_uuid: [u8; 16],
    },
}

/// A checked-out texture: what an importer needs to rebuild the exporter's
/// image on its own device and join the crossing's timeline pair.
pub struct CheckedOutTexture {
    pub width: u32,
    pub height: u32,
    /// The wire name of the texture format (`"rgba8_unorm"`, …).
    pub format_wire_name: String,
    /// The exporter's allocation size, which an importer binds at least.
    pub allocation_byte_size: u64,
    /// The `VkImageLayout` the last holder published, as its raw value.
    pub current_image_layout: i32,
    pub memory: CheckedOutTextureMemory,
    /// The producer's timeline edge, when the registration carries one.
    pub produce_done_timeline_fd: Option<OwnedFd>,
    /// The consumers' timeline edge, when the registration carries one.
    pub consume_done_timeline_fd: Option<OwnedFd>,
}

impl CheckedOutTexture {
    /// Read a texture's `check_out` answer and the fds it delivered.
    pub fn from_check_out(
        surface_id: &str,
        response: &serde_json::Value,
        check_out_fds: SurfaceShareCheckOutFds,
    ) -> Result<Self, String> {
        let SurfaceShareCheckOutFds {
            plane_fds,
            produce_done_fd,
            consume_done_fd,
        } = check_out_fds;
        let [memory_fd] = <[OwnedFd; 1]>::try_from(plane_fds).map_err(|delivered| {
            format!(
                "texture {surface_id:?} arrived with {} memory fds; a texture checkout carries \
                 its memory as exactly one",
                delivered.len()
            )
        })?;
        let width = required_positive_u32(response, surface_id, "width")?;
        let height = required_positive_u32(response, surface_id, "height")?;
        let format_wire_name = response
            .get("format")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("texture {surface_id:?} was registered without its format"))?
            .to_owned();
        let allocation_byte_size = response
            .get("vk_image_allocation_size")
            .and_then(serde_json::Value::as_u64)
            .filter(|size| *size > 0)
            .ok_or_else(|| {
                format!(
                    "texture {surface_id:?} was registered without its allocation byte size; \
                     binding imported memory of unknown extent reads past the allocation \
                     instead of failing here"
                )
            })?;
        let current_image_layout_raw = response
            .get("current_image_layout")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let current_image_layout = i32::try_from(current_image_layout_raw).map_err(|_| {
            format!(
                "texture {surface_id:?} reports image layout {current_image_layout_raw}, which \
                 is not a VkImageLayout"
            )
        })?;
        let memory = match response
            .get("handle_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("dma_buf")
        {
            "dma_buf" => {
                let drm_format_modifier = response
                    .get("drm_format_modifier")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|modifier| *modifier != 0)
                    .ok_or_else(|| {
                        format!(
                            "texture {surface_id:?} was registered without an explicit DRM \
                             modifier: its image layout is driver-opaque and no other process \
                             can reconstruct it"
                        )
                    })?;
                CheckedOutTextureMemory::DmaBufWithExplicitDrmFormatModifier {
                    memory_fd,
                    plane_offsets: u64_array(response, "plane_offsets"),
                    plane_strides: u64_array(response, "plane_strides"),
                    drm_format_modifier,
                }
            }
            "opaque_fd" => {
                let vk_memory_type_index = response
                    .get("vk_memory_type_index")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|index| u32::try_from(index).ok())
                    .ok_or_else(|| {
                        format!(
                            "opaque_fd texture {surface_id:?} was registered without a usable \
                             vk_memory_type_index; an OPAQUE_FD import must bind the memory \
                             type index the exporter allocated from"
                        )
                    })?;
                let exporting_device_uuid = response
                    .get("exporting_device_uuid")
                    .and_then(serde_json::Value::as_str)
                    .and_then(device_uuid_of_its_hex_spelling)
                    .ok_or_else(|| {
                        format!(
                            "opaque_fd texture {surface_id:?} was registered without a usable \
                             exporting device UUID"
                        )
                    })?;
                let vk_image_usage_flags = response
                    .get("vk_image_usage")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|usage| u32::try_from(usage).ok())
                    .unwrap_or(VK_IMAGE_USAGE_OF_A_REGISTRATION_THAT_STATES_NONE);
                CheckedOutTextureMemory::OpaqueFd {
                    memory_fd,
                    vk_image_usage_flags,
                    vk_memory_type_index,
                    exporting_device_uuid,
                }
            }
            other => {
                return Err(format!(
                    "texture {surface_id:?} is registered with handle type {other:?}, which this \
                     consumer does not know; a texture crosses as dma_buf or opaque_fd"
                ));
            }
        };
        Ok(Self {
            width,
            height,
            format_wire_name,
            allocation_byte_size,
            current_image_layout,
            memory,
            produce_done_timeline_fd: produce_done_fd,
            consume_done_timeline_fd: consume_done_fd,
        })
    }
}

/// Claim `surface_id` against producer reuse and read it as a texture. The
/// claim holds until `release_check_out_on_surface_share` on the same
/// connection, or until the connection closes.
pub fn check_out_texture_on_surface_share(
    surface_share_connection: &UnixStream,
    surface_id: &str,
) -> Result<CheckedOutTexture, String> {
    let (response, received_raw_fds) = send_request_with_fds(
        surface_share_connection,
        &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
        &[],
        MAX_SCM_RIGHTS_FDS,
    )
    .map_err(|request_failure| format!("check_out of {surface_id:?}: {request_failure}"))?;
    // SAFETY: each fd arrived over SCM_RIGHTS and is owned by nothing else.
    let received_fds: Vec<OwnedFd> = received_raw_fds
        .into_iter()
        .map(|raw_fd| unsafe { OwnedFd::from_raw_fd(raw_fd) })
        .collect();
    if let Some(refusal) = refusal_the_surface_share_service_answered(&response) {
        return Err(format!(
            "the surface-share service refused check_out of {surface_id:?}: {refusal}"
        ));
    }
    let resource_type = resource_type_of_a_check_out(&response);
    if resource_type != "texture" {
        return Err(format!(
            "surface {surface_id:?} is registered as resource type {resource_type:?}; only a \
             texture is read here"
        ));
    }
    let check_out_fds =
        SurfaceShareCheckOutFds::split_off_the_timeline_fds(surface_id, &response, received_fds)?;
    CheckedOutTexture::from_check_out(surface_id, &response, check_out_fds)
}

/// Publish the layout a producer's release barrier left `surface_id` in, so
/// the next holder's acquire names the right source layout.
pub fn publish_image_layout_on_surface_share(
    surface_share_connection: &UnixStream,
    surface_id: &str,
    current_image_layout: i32,
) -> Result<(), String> {
    let (response, _no_fds) = send_request_with_fds(
        surface_share_connection,
        &serde_json::json!({
            "op": "update_layout",
            "surface_id": surface_id,
            "current_image_layout": current_image_layout,
        }),
        &[],
        0,
    )
    .map_err(|request_failure| format!("update_layout of {surface_id:?}: {request_failure}"))?;
    if let Some(refusal) = refusal_the_surface_share_service_answered(&response) {
        return Err(format!(
            "the surface-share service refused update_layout of {surface_id:?}: {refusal}"
        ));
    }
    match response.get("success").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(format!(
            "the surface-share service recorded no layout for {surface_id:?}: it knows no such \
             registration"
        )),
    }
}

fn required_positive_u32(
    response: &serde_json::Value,
    surface_id: &str,
    field: &str,
) -> Result<u32, String> {
    response
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("check_out of {surface_id:?} carried no usable {field}"))
}

fn u64_array(response: &serde_json::Value, field: &str) -> Vec<u64> {
    response
        .get(field)
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_u64)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe_fd() -> OwnedFd {
        let (read_end, _write_end) = std::io::pipe().expect("a pipe opens");
        OwnedFd::from(read_end)
    }

    fn one_memory_fd() -> SurfaceShareCheckOutFds {
        SurfaceShareCheckOutFds {
            plane_fds: vec![pipe_fd()],
            produce_done_fd: Some(pipe_fd()),
            consume_done_fd: None,
        }
    }

    #[test]
    fn a_render_target_dma_buf_reads_its_modifier_planes_and_published_layout() {
        let response = serde_json::json!({
            "resource_type": "texture", "width": 1920, "height": 1080,
            "format": "rgba8_unorm", "handle_type": "dma_buf",
            "drm_format_modifier": 0x0300_0000_0060_6014_u64,
            "plane_offsets": [0], "plane_strides": [7680],
            "vk_image_allocation_size": 8_388_608, "current_image_layout": 5,
        });
        let texture = CheckedOutTexture::from_check_out("t", &response, one_memory_fd()).unwrap();
        assert_eq!((texture.width, texture.height), (1920, 1080));
        assert_eq!(texture.format_wire_name, "rgba8_unorm");
        assert_eq!(texture.current_image_layout, 5);
        assert!(texture.produce_done_timeline_fd.is_some());
        assert!(texture.consume_done_timeline_fd.is_none());
        let CheckedOutTextureMemory::DmaBufWithExplicitDrmFormatModifier {
            plane_offsets,
            plane_strides,
            drm_format_modifier,
            ..
        } = texture.memory
        else {
            panic!("a DMA-BUF texture");
        };
        assert_eq!(plane_offsets, vec![0]);
        assert_eq!(plane_strides, vec![7680]);
        assert_eq!(drm_format_modifier, 0x0300_0000_0060_6014);
    }

    #[test]
    fn a_dma_buf_texture_without_an_explicit_modifier_is_refused_by_name() {
        let response = serde_json::json!({
            "width": 4, "height": 4, "format": "rgba8_unorm", "handle_type": "dma_buf",
            "vk_image_allocation_size": 64,
        });
        let refusal = CheckedOutTexture::from_check_out("t", &response, one_memory_fd())
            .err()
            .expect("refused");
        assert!(refusal.contains("explicit DRM modifier"), "{refusal}");
    }

    #[test]
    fn an_opaque_fd_texture_reads_its_usage_and_memory_type() {
        let response = serde_json::json!({
            "width": 4, "height": 4, "format": "rgba8_unorm", "handle_type": "opaque_fd",
            "vk_image_allocation_size": 64, "vk_memory_type_index": 3, "vk_image_usage": 0x1F,
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
        });
        let texture = CheckedOutTexture::from_check_out("t", &response, one_memory_fd()).unwrap();
        let CheckedOutTextureMemory::OpaqueFd {
            vk_image_usage_flags,
            vk_memory_type_index,
            exporting_device_uuid,
            ..
        } = texture.memory
        else {
            panic!("an opaque_fd texture");
        };
        assert_eq!(vk_image_usage_flags, 0x1F);
        assert_eq!(vk_memory_type_index, 3);
        assert_eq!(exporting_device_uuid[15], 0xff);
    }

    #[test]
    fn a_texture_without_its_allocation_size_is_refused_by_name() {
        let response = serde_json::json!({
            "width": 4, "height": 4, "format": "rgba8_unorm", "handle_type": "dma_buf",
            "drm_format_modifier": 7,
        });
        let refusal = CheckedOutTexture::from_check_out("t", &response, one_memory_fd())
            .err()
            .expect("refused");
        assert!(refusal.contains("allocation byte size"), "{refusal}");
    }

    #[test]
    fn a_texture_crossing_with_two_memory_fds_is_refused() {
        let check_out_fds = SurfaceShareCheckOutFds {
            plane_fds: vec![pipe_fd(), pipe_fd()],
            produce_done_fd: None,
            consume_done_fd: None,
        };
        let response = serde_json::json!({"width": 4, "height": 4, "format": "rgba8_unorm"});
        let refusal = CheckedOutTexture::from_check_out("t", &response, check_out_fds)
            .err()
            .expect("refused");
        assert!(refusal.contains("exactly one"), "{refusal}");
    }
}
