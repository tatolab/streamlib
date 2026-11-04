// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A surface-share `check_out` answer, read into typed fields: which fds are
//! plane memory and which are timeline semaphores, what resource the surface
//! is, and its extent, layout facts and the handle type its memory crossed
//! as.

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;

use crate::linux::{MAX_SCM_RIGHTS_FDS, send_request_with_fds};

/// The fds a `check_out` answer delivered, split into plane memory and the
/// trailing timeline semaphores the registration states it carries.
pub struct SurfaceShareCheckOutFds {
    pub plane_fds: Vec<OwnedFd>,
    pub produce_done_fd: Option<OwnedFd>,
    pub consume_done_fd: Option<OwnedFd>,
}

/// The handle type a checked-out pixel buffer's memory crossed as.
pub enum CheckedOutPixelBufferMemory {
    /// DMA-BUF planes, one fd per plane.
    DmaBuf { plane_fds: Vec<OwnedFd> },
    /// One OPAQUE_FD allocation, imported at the memory type index the
    /// exporter allocated from, on the GPU the exporter named.
    OpaqueFd {
        memory_fd: OwnedFd,
        vk_memory_type_index: u32,
        exporting_device_uuid: [u8; 16],
    },
}

/// A checked-out pixel buffer: its layout facts and its memory's handles.
pub struct CheckedOutPixelBuffer {
    pub width: u32,
    pub height: u32,
    /// The wire name of the pixel format (`"rgba32"`, `"bgra32"`, …).
    pub format_wire_name: String,
    pub plane_sizes: Vec<u64>,
    /// The allocation's row pitch, padding included.
    pub bytes_per_row: u64,
    pub memory: CheckedOutPixelBufferMemory,
}

/// The surface-share field a registration states its pixel planes' row
/// pitches in, first plane first.
pub const SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD: &str = "pixel_plane_strides";

/// The `check_out` request field a helper sets when it already holds its own
/// import of the published frame's pool slot; the service then answers the
/// frame's metadata and sends no handle.
pub const SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD: &str = "holds_pool_slot_import";

/// The reply field listing the pool slot keys the engine unregistered since
/// this connection was last answered, among those it handed memory for.
pub const SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD: &str = "retired_pool_slots";

/// The first plane's row pitch a registration states, padding included;
/// `None` when it states no plane layout.
fn first_pixel_plane_stride_a_registration_states(response: &serde_json::Value) -> Option<u64> {
    response
        .get(SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD)?
        .as_array()?
        .first()?
        .as_u64()
        .filter(|&stride| stride > 0)
}

/// The refusal the service answered a request with, if it answered one.
pub fn refusal_the_surface_share_service_answered(response: &serde_json::Value) -> Option<&str> {
    response.get("error").and_then(|value| value.as_str())
}

/// The resource type a `check_out` answer names; a registration that names
/// none is a pixel buffer.
pub fn resource_type_of_a_check_out(response: &serde_json::Value) -> &str {
    response
        .get("resource_type")
        .and_then(|value| value.as_str())
        .unwrap_or("pixel_buffer")
}

impl SurfaceShareCheckOutFds {
    /// Split `received_fds` into plane fds and the trailing timeline fds the
    /// answer's `has_produce_done_fd` / `has_consume_done_fd` state.
    pub fn split_off_the_timeline_fds(
        surface_id: &str,
        response: &serde_json::Value,
        received_fds: Vec<OwnedFd>,
    ) -> Result<Self, String> {
        let has_produce_done_fd =
            registration_states_a_timeline_fd(response, "has_produce_done_fd");
        let has_consume_done_fd =
            registration_states_a_timeline_fd(response, "has_consume_done_fd");
        let trailing_timeline_fd_count =
            usize::from(has_produce_done_fd) + usize::from(has_consume_done_fd);
        if received_fds.len() < trailing_timeline_fd_count + 1 {
            return Err(format!(
                "check_out of {surface_id:?} returned {} fds, fewer than the {} its metadata \
                 promises",
                received_fds.len(),
                trailing_timeline_fd_count + 1,
            ));
        }
        let mut plane_fds = received_fds;
        let mut trailing_timeline_fds = plane_fds
            .split_off(plane_fds.len() - trailing_timeline_fd_count)
            .into_iter();
        let produce_done_fd = has_produce_done_fd
            .then(|| trailing_timeline_fds.next())
            .flatten();
        let consume_done_fd = has_consume_done_fd
            .then(|| trailing_timeline_fds.next())
            .flatten();
        Ok(Self {
            plane_fds,
            produce_done_fd,
            consume_done_fd,
        })
    }
}

impl CheckedOutPixelBuffer {
    /// Read a pixel buffer's `check_out` answer and its plane fds.
    pub fn from_check_out(
        surface_id: &str,
        response: &serde_json::Value,
        plane_fds: Vec<OwnedFd>,
    ) -> Result<Self, String> {
        let delivered_plane_fd_count = plane_fds.len();
        let memory = match response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf")
        {
            "dma_buf" => CheckedOutPixelBufferMemory::DmaBuf { plane_fds },
            "opaque_fd" => {
                let [memory_fd] =
                    <[OwnedFd; 1]>::try_from(plane_fds).map_err(|delivered: Vec<OwnedFd>| {
                        format!(
                            "pixel buffer {surface_id:?} is registered as opaque_fd with {} \
                             planes; an OPAQUE_FD pixel buffer imports as one allocation at its \
                             stated memory type index, so only a single-plane one is accepted",
                            delivered.len()
                        )
                    })?;
                let registered_subject = format!("opaque_fd pixel buffer {surface_id:?}");
                CheckedOutPixelBufferMemory::OpaqueFd {
                    memory_fd,
                    vk_memory_type_index: memory_type_index_stated_by_an_opaque_fd_registration(
                        &registered_subject,
                        response,
                    )?,
                    exporting_device_uuid: exporting_device_uuid_stated_by_a_registration(
                        &registered_subject,
                        response,
                    )?,
                }
            }
            other => {
                return Err(format!(
                    "pixel buffer {surface_id:?} is registered with handle type {other:?}, \
                     which this consumer does not know; a pixel buffer crosses as dma_buf or \
                     opaque_fd"
                ));
            }
        };
        let width = required_positive_u32_check_out_metadata_field(response, surface_id, "width")?;
        let height =
            required_positive_u32_check_out_metadata_field(response, surface_id, "height")?;
        let format_wire_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string();
        let plane_sizes = plane_u64_array_check_out_metadata_field(response, "plane_sizes");
        if plane_sizes.len() != delivered_plane_fd_count {
            return Err(format!(
                "check_out of {surface_id:?} returned {delivered_plane_fd_count} plane fds but {} \
                 plane sizes",
                plane_sizes.len(),
            ));
        }
        let plane0_size = plane_sizes.first().copied().unwrap_or(0);
        let bytes_per_row = match first_pixel_plane_stride_a_registration_states(response) {
            Some(stated_first_plane_stride) => stated_first_plane_stride,
            None => {
                if plane0_size == 0 || !plane0_size.is_multiple_of(u64::from(height)) {
                    return Err(format!(
                        "surface {surface_id:?} reports plane size {plane0_size}, not a whole \
                         number of {height} rows"
                    ));
                }
                plane0_size / u64::from(height)
            }
        };
        Ok(Self {
            width,
            height,
            format_wire_name,
            bytes_per_row,
            plane_sizes,
            memory,
        })
    }
}

/// The `vk_image_usage` a texture registration that states none carries:
/// `TRANSFER_SRC | TRANSFER_DST | SAMPLED | STORAGE`.
pub const SURFACE_SHARE_TEXTURE_IMAGE_USAGE_DEFAULT: u32 = 0x0F;

/// The handle type a checked-out texture's memory crossed as. Either way the
/// whole image travels as one memory fd.
pub enum CheckedOutTextureMemory {
    /// A DMA-BUF with an explicit DRM format modifier, its planes at the
    /// stated offsets and row pitches.
    DmaBuf {
        memory_fd: OwnedFd,
        drm_format_modifier: u64,
        plane_offsets: Vec<u64>,
        plane_strides: Vec<u64>,
    },
    /// One OPAQUE_FD allocation, imported at the memory type index the
    /// exporter allocated from, on the GPU the exporter named.
    OpaqueFd {
        memory_fd: OwnedFd,
        vk_memory_type_index: u32,
        exporting_device_uuid: [u8; 16],
    },
}

/// A checked-out texture: the image its registration describes and its
/// memory's handle.
pub struct CheckedOutTexture {
    pub width: u32,
    pub height: u32,
    /// The wire name of the texture format (`"rgba8_unorm"`, …).
    pub format_wire_name: String,
    pub allocation_byte_size: u64,
    /// The `VkImageLayout` its last user published, `UNDEFINED` (0) when
    /// none has.
    pub current_image_layout: i32,
    pub vk_image_usage_flags: u32,
    pub memory: CheckedOutTextureMemory,
    /// The producer's produce-done timeline, when the registration carries one.
    pub produce_done_timeline_fd: Option<OwnedFd>,
    /// The consumers' consume-done timeline, when the registration carries one.
    pub consume_done_timeline_fd: Option<OwnedFd>,
}

impl CheckedOutTexture {
    /// Read a texture's `check_out` answer and its plane fds.
    pub fn from_check_out(
        surface_id: &str,
        response: &serde_json::Value,
        plane_fds: Vec<OwnedFd>,
    ) -> Result<Self, String> {
        let width = required_positive_u32_check_out_metadata_field(response, surface_id, "width")?;
        let height =
            required_positive_u32_check_out_metadata_field(response, surface_id, "height")?;
        let format_wire_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string();
        let allocation_byte_size = response
            .get("vk_image_allocation_size")
            .and_then(|value| value.as_u64())
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
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let current_image_layout = i32::try_from(current_image_layout_raw).map_err(|_| {
            format!(
                "texture {surface_id:?} reports image layout {current_image_layout_raw}, which \
                 is not a VkImageLayout"
            )
        })?;
        let vk_image_usage_flags = response
            .get("vk_image_usage")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(SURFACE_SHARE_TEXTURE_IMAGE_USAGE_DEFAULT);
        let [memory_fd] =
            <[OwnedFd; 1]>::try_from(plane_fds).map_err(|delivered: Vec<OwnedFd>| {
                format!(
                    "texture {surface_id:?} arrived with {} fds; a texture checkout carries its \
                 memory as exactly one",
                    delivered.len()
                )
            })?;
        let memory = match response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf")
        {
            "dma_buf" => {
                let drm_format_modifier = response
                    .get("drm_format_modifier")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if drm_format_modifier == 0 {
                    return Err(format!(
                        "texture {surface_id:?} was registered without an explicit DRM \
                         modifier: its image layout is driver-opaque and no other process can \
                         reconstruct it"
                    ));
                }
                CheckedOutTextureMemory::DmaBuf {
                    memory_fd,
                    drm_format_modifier,
                    plane_offsets: plane_u64_array_check_out_metadata_field(
                        response,
                        "plane_offsets",
                    ),
                    plane_strides: plane_u64_array_check_out_metadata_field(
                        response,
                        "plane_strides",
                    ),
                }
            }
            "opaque_fd" => {
                let registered_subject = format!("opaque_fd texture {surface_id:?}");
                CheckedOutTextureMemory::OpaqueFd {
                    memory_fd,
                    vk_memory_type_index: memory_type_index_stated_by_an_opaque_fd_registration(
                        &registered_subject,
                        response,
                    )?,
                    exporting_device_uuid: exporting_device_uuid_stated_by_a_registration(
                        &registered_subject,
                        response,
                    )?,
                }
            }
            other => {
                return Err(format!(
                    "texture {surface_id:?} is registered with handle type {other:?}, which \
                     this consumer does not know"
                ));
            }
        };
        Ok(Self {
            width,
            height,
            format_wire_name,
            allocation_byte_size,
            current_image_layout,
            vk_image_usage_flags,
            memory,
            produce_done_timeline_fd: None,
            consume_done_timeline_fd: None,
        })
    }
}

/// A checked-out surface, read as the resource its registration names.
pub enum CheckedOutSurface {
    PixelBuffer(CheckedOutPixelBuffer),
    Texture(CheckedOutTexture),
}

/// Claim `surface_id` against producer reuse and read it as the pixel buffer
/// or texture it is registered as. The claim holds until
/// [`release_check_out_on_surface_share`] on the same connection, or until the
/// connection closes.
pub fn check_out_surface_on_surface_share(
    surface_share_connection: &UnixStream,
    surface_id: &str,
) -> Result<CheckedOutSurface, String> {
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
    let check_out_fds =
        SurfaceShareCheckOutFds::split_off_the_timeline_fds(surface_id, &response, received_fds)?;
    match resource_type_of_a_check_out(&response) {
        "pixel_buffer" => {
            CheckedOutPixelBuffer::from_check_out(surface_id, &response, check_out_fds.plane_fds)
                .map(CheckedOutSurface::PixelBuffer)
        }
        "texture" => {
            let mut texture =
                CheckedOutTexture::from_check_out(surface_id, &response, check_out_fds.plane_fds)?;
            texture.produce_done_timeline_fd = check_out_fds.produce_done_fd;
            texture.consume_done_timeline_fd = check_out_fds.consume_done_fd;
            Ok(CheckedOutSurface::Texture(texture))
        }
        other => Err(format!(
            "surface {surface_id:?} is registered as resource type {other:?}; a pixel buffer or \
             a texture is read here"
        )),
    }
}

/// Publish the `VkImageLayout` a checked-out texture was left in, so its next
/// reader's acquire barrier names the right source layout.
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
            "the surface-share service refused the layout publish for {surface_id:?}: {refusal}"
        ));
    }
    match response.get("success").and_then(|value| value.as_bool()) {
        Some(true) => Ok(()),
        _ => Err(format!(
            "the surface-share service did not record the layout publish for {surface_id:?}"
        )),
    }
}

/// Let go of one claim on `surface_id`, freeing its slot for its producer.
pub fn release_check_out_on_surface_share(
    surface_share_connection: &UnixStream,
    surface_id: &str,
) -> Result<(), String> {
    let (response, _no_fds) = send_request_with_fds(
        surface_share_connection,
        &serde_json::json!({"op": "release_check_out", "surface_id": surface_id}),
        &[],
        0,
    )
    .map_err(|request_failure| format!("release_check_out of {surface_id:?}: {request_failure}"))?;
    match refusal_the_surface_share_service_answered(&response) {
        Some(refusal) => Err(format!(
            "the surface-share service refused release_check_out of {surface_id:?}: {refusal}"
        )),
        None => Ok(()),
    }
}

fn registration_states_a_timeline_fd(registration: &serde_json::Value, flag: &str) -> bool {
    registration
        .get(flag)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn required_positive_u32_check_out_metadata_field(
    response: &serde_json::Value,
    surface_id: &str,
    field: &str,
) -> Result<u32, String> {
    response
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("check_out of {surface_id:?} carried no usable {field}"))
}

fn plane_u64_array_check_out_metadata_field(response: &serde_json::Value, field: &str) -> Vec<u64> {
    response
        .get(field)
        .and_then(|value| value.as_array())
        .map(|entries| entries.iter().filter_map(|entry| entry.as_u64()).collect())
        .unwrap_or_default()
}

/// An OPAQUE_FD import must bind the memory type index the exporter
/// allocated from; a guess binds the wrong memory instead of failing.
fn memory_type_index_stated_by_an_opaque_fd_registration(
    registered_subject: &str,
    registration: &serde_json::Value,
) -> Result<u32, String> {
    registration
        .get("vk_memory_type_index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or_else(|| {
            format!(
                "{registered_subject} was registered without a usable vk_memory_type_index; an \
                 OPAQUE_FD import must bind the memory type index the exporter allocated from, \
                 and guessing one binds the wrong memory instead of failing"
            )
        })
}

fn exporting_device_uuid_stated_by_a_registration(
    registered_subject: &str,
    registration: &serde_json::Value,
) -> Result<[u8; 16], String> {
    let as_hex = registration
        .get("exporting_device_uuid")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            format!(
                "{registered_subject} was registered without its exporting device UUID; \
                 importing onto the wrong GPU of a multi-GPU rig reads the wrong memory \
                 instead of failing here"
            )
        })?;
    device_uuid_of_its_hex_spelling(as_hex)
        .ok_or_else(|| format!("{registered_subject} names a malformed device UUID {as_hex:?}"))
}

/// A 16-byte device UUID spelled as 32 hex digits.
pub fn device_uuid_of_its_hex_spelling(as_hex: &str) -> Option<[u8; 16]> {
    if as_hex.len() != 32 {
        return None;
    }
    let mut device_uuid = [0u8; 16];
    for (byte_index, byte) in device_uuid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(as_hex.get(byte_index * 2..byte_index * 2 + 2)?, 16).ok()?;
    }
    Some(device_uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe_fd() -> OwnedFd {
        let (read_end, _write_end) = std::io::pipe().expect("a pipe opens");
        OwnedFd::from(read_end)
    }

    fn texture_check_out_response(handle_type: &str) -> serde_json::Value {
        serde_json::json!({
            "resource_type": "texture", "width": 8, "height": 4, "format": "rgba8_unorm",
            "handle_type": handle_type, "vk_image_allocation_size": 4096,
            "current_image_layout": 1, "drm_format_modifier": 3,
            "plane_offsets": [0], "plane_strides": [64], "vk_memory_type_index": 2,
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
        })
    }

    #[test]
    fn a_dma_buf_texture_reads_its_modifier_planes_and_published_layout() {
        let texture = CheckedOutTexture::from_check_out(
            "s",
            &texture_check_out_response("dma_buf"),
            vec![pipe_fd()],
        )
        .unwrap();
        assert_eq!((texture.width, texture.height), (8, 4));
        assert_eq!(texture.current_image_layout, 1);
        assert_eq!(
            texture.vk_image_usage_flags,
            SURFACE_SHARE_TEXTURE_IMAGE_USAGE_DEFAULT
        );
        assert!(matches!(
            texture.memory,
            CheckedOutTextureMemory::DmaBuf { drm_format_modifier: 3, ref plane_strides, .. }
                if plane_strides == &[64]
        ));
    }

    #[test]
    fn an_opaque_fd_texture_reads_its_export_contract() {
        let texture = CheckedOutTexture::from_check_out(
            "s",
            &texture_check_out_response("opaque_fd"),
            vec![pipe_fd()],
        )
        .unwrap();
        assert!(matches!(
            texture.memory,
            CheckedOutTextureMemory::OpaqueFd {
                vk_memory_type_index: 2,
                ..
            }
        ));
    }

    #[test]
    fn a_dma_buf_texture_without_an_explicit_modifier_is_refused_by_name() {
        let mut response = texture_check_out_response("dma_buf");
        response["drm_format_modifier"] = 0.into();
        let refusal = CheckedOutTexture::from_check_out("s", &response, vec![pipe_fd()])
            .err()
            .expect("refused");
        assert!(refusal.contains("explicit DRM modifier"), "{refusal}");
    }

    #[test]
    fn a_texture_delivered_as_more_than_one_fd_is_refused() {
        let refusal = CheckedOutTexture::from_check_out(
            "s",
            &texture_check_out_response("dma_buf"),
            vec![pipe_fd(), pipe_fd()],
        )
        .err()
        .expect("refused");
        assert!(refusal.contains("arrived with 2 fds"), "{refusal}");
    }

    #[test]
    fn a_stated_plane_stride_is_the_row_pitch_even_past_whole_rows() {
        let response = serde_json::json!({
            "width": 4, "height": 2, "format": "nv12", "plane_sizes": [4100],
            "handle_type": "dma_buf", "pixel_plane_offsets": [0, 128],
            "pixel_plane_strides": [64, 64],
        });
        let pixel_buffer =
            CheckedOutPixelBuffer::from_check_out("s", &response, vec![pipe_fd()]).unwrap();
        assert_eq!(pixel_buffer.bytes_per_row, 64);
    }

    #[test]
    fn a_dma_buf_pixel_buffer_reads_its_extent_pitch_and_planes() {
        let response = serde_json::json!({
            "width": 4, "height": 2, "format": "rgba8", "plane_sizes": [64],
            "handle_type": "dma_buf",
        });
        let pixel_buffer =
            CheckedOutPixelBuffer::from_check_out("s", &response, vec![pipe_fd()]).unwrap();
        assert_eq!((pixel_buffer.width, pixel_buffer.height), (4, 2));
        assert_eq!(pixel_buffer.bytes_per_row, 32);
        assert_eq!(pixel_buffer.format_wire_name, "rgba8");
        assert!(matches!(
            pixel_buffer.memory,
            CheckedOutPixelBufferMemory::DmaBuf { ref plane_fds } if plane_fds.len() == 1
        ));
    }

    #[test]
    fn an_opaque_fd_pixel_buffer_without_its_memory_type_index_is_refused_by_name() {
        let response = serde_json::json!({
            "width": 4, "height": 2, "plane_sizes": [64], "handle_type": "opaque_fd",
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
        });
        let refusal = CheckedOutPixelBuffer::from_check_out("s", &response, vec![pipe_fd()])
            .err()
            .expect("refused");
        assert!(refusal.contains("vk_memory_type_index"), "{refusal}");
    }

    #[test]
    fn an_opaque_fd_pixel_buffer_reads_its_export_contract() {
        let response = serde_json::json!({
            "width": 4, "height": 2, "plane_sizes": [64], "handle_type": "opaque_fd",
            "vk_memory_type_index": 7,
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
        });
        let pixel_buffer =
            CheckedOutPixelBuffer::from_check_out("s", &response, vec![pipe_fd()]).unwrap();
        let CheckedOutPixelBufferMemory::OpaqueFd {
            vk_memory_type_index,
            exporting_device_uuid,
            ..
        } = pixel_buffer.memory
        else {
            panic!("an opaque_fd pixel buffer");
        };
        assert_eq!(vk_memory_type_index, 7);
        assert_eq!(exporting_device_uuid[15], 0xff);
    }

    #[test]
    fn plane_sizes_that_do_not_match_the_delivered_fds_are_refused() {
        let response = serde_json::json!({"width": 4, "height": 2, "plane_sizes": [64, 32]});
        let refusal = CheckedOutPixelBuffer::from_check_out("s", &response, vec![pipe_fd()])
            .err()
            .expect("refused");
        assert!(
            refusal.contains("1 plane fds but 2 plane sizes"),
            "{refusal}"
        );
    }

    #[test]
    fn a_plane_that_is_not_a_whole_number_of_rows_is_refused() {
        let response = serde_json::json!({"width": 4, "height": 3, "plane_sizes": [64]});
        let refusal = CheckedOutPixelBuffer::from_check_out("s", &response, vec![pipe_fd()])
            .err()
            .expect("refused");
        assert!(
            refusal.contains("not a whole number of 3 rows"),
            "{refusal}"
        );
    }

    #[test]
    fn trailing_timeline_fds_are_split_off_the_plane_fds() {
        let response =
            serde_json::json!({"has_produce_done_fd": true, "has_consume_done_fd": true});
        let fds = SurfaceShareCheckOutFds::split_off_the_timeline_fds(
            "s",
            &response,
            vec![pipe_fd(), pipe_fd(), pipe_fd()],
        )
        .unwrap();
        assert_eq!(fds.plane_fds.len(), 1);
        assert!(fds.produce_done_fd.is_some() && fds.consume_done_fd.is_some());
    }

    #[test]
    fn fewer_fds_than_the_registration_promises_are_refused() {
        let response = serde_json::json!({"has_produce_done_fd": true});
        let refusal =
            SurfaceShareCheckOutFds::split_off_the_timeline_fds("s", &response, vec![pipe_fd()])
                .err()
                .expect("refused");
        assert!(refusal.contains("fewer than the 2"), "{refusal}");
    }

    #[test]
    fn a_device_uuid_reads_from_its_hex_spelling() {
        assert_eq!(
            device_uuid_of_its_hex_spelling("000102030405060708090a0b0c0d0e0f"),
            Some([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
        assert_eq!(device_uuid_of_its_hex_spelling("00"), None);
        assert_eq!(
            device_uuid_of_its_hex_spelling("zz0102030405060708090a0b0c0d0e0f"),
            None
        );
    }
}
