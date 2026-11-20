// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_ipc_types::{
    GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody, ImageFrameH273Colour,
};

use super::rgba_or_bgra_image_frame_to_pooled_rgba_conversion_stage::RgbaOrBgraImageFrameToPooledRgbaConversionStage;
use crate::core::Error;
use crate::core::context::{
    GpuContext, GpuContextLimitedAccess, GpuPoolClaimTableRowLease, GpuPoolClaimTablesOfOnePool,
    NodeOutputPixelBufferPoolOfOneNativeNode,
};
use crate::core::rhi::{
    TextureDescriptor, TextureFormat, TextureUsages, VulkanLayout,
    texture_format_the_image_to_image_kernel_samples,
};
use crate::core::test_support::LiveSurfaceShareServiceForATest;
use crate::vulkan::rhi::ImageCopyRegion;

const FRAME_COLOUR: ImageFrameH273Colour = ImageFrameH273Colour {
    primaries: 1,
    transfer: 13,
    matrix: 0,
    range: 1,
};

/// A frame of a format the kernel would not sample as it is stored, or no format at all, is
/// refused naming it and the two it takes.
#[test]
fn a_frame_of_a_format_the_kernel_does_not_sample_as_stored_is_refused_naming_it() {
    for (format_wire_value, named) in [
        (TextureFormat::Rgba16Float as u32, "rgba16_float"),
        (TextureFormat::Bgra8UnormSrgb as u32, "bgra8_unorm_srgb"),
        (TextureFormat::Nv12 as u32, "nv12"),
        (9_999, "an unknown texture format (9999)"),
    ] {
        let refusal = texture_format_the_image_to_image_kernel_samples(format_wire_value)
            .expect_err("the kernel does not sample it as stored");
        assert!(matches!(refusal, Error::NotSupported(_)), "{refusal}");
        let refusal = refusal.to_string();
        assert!(refusal.contains(named), "{refusal}");
        assert!(refusal.contains("rgba8_unorm and bgra8_unorm"), "{refusal}");
    }
    for format in [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm] {
        assert_eq!(
            texture_format_the_image_to_image_kernel_samples(format as u32)
                .expect("the kernel samples it"),
            format
        );
    }
}

/// A GPU, a live surface-share service as its store, and a stage built under full access.
struct ConversionUnderTest {
    gpu: GpuContext,
    limited: GpuContextLimitedAccess,
    live_service: LiveSurfaceShareServiceForATest,
    stage: RgbaOrBgraImageFrameToPooledRgbaConversionStage,
    producer_pool: NodeOutputPixelBufferPoolOfOneNativeNode,
}

/// The fixture, or `None` with the skip printed when no GPU device is present.
fn a_conversion_under_test(test_name: &str) -> Option<ConversionUnderTest> {
    let gpu = match GpuContext::init_for_platform() {
        Ok(gpu) => gpu,
        Err(no_device) => {
            eprintln!("SKIPPED {test_name}: no GPU device ({no_device})");
            return None;
        }
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let limited = GpuContextLimitedAccess::new(gpu.clone());
    let stage = RgbaOrBgraImageFrameToPooledRgbaConversionStage::create(
        &limited.to_full_access(),
        "a-texture-format-convert",
    )
    .expect("the stage");
    let producer_pool = gpu.create_node_output_pixel_buffer_pool_for_a_native_node("a-producer");
    Some(ConversionUnderTest {
        gpu,
        limited,
        live_service,
        stage,
        producer_pool,
    })
}

/// Pixels of `width` by `height` in their stored byte order, no two neighbours alike.
fn stored_pixels_of(width: u32, height: u32) -> Vec<u8> {
    (0..width * height)
        .flat_map(|pixel| {
            let (x, y) = (pixel % width, pixel / width);
            [
                (x * 7 + y * 3) as u8,
                (255 - x * 5) as u8,
                (y * 37 + 11) as u8,
                (128 + x) as u8,
            ]
        })
        .collect()
}

/// `stored` read as `format`, in RGBA order.
fn in_rgba_order(stored: &[u8], format: TextureFormat) -> Vec<u8> {
    match format {
        TextureFormat::Bgra8Unorm => stored
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[blue, green, red, alpha]| [red, green, blue, alpha])
            .collect(),
        _ => stored.to_vec(),
    }
}

impl ConversionUnderTest {
    /// An Image frame of `format` on a pixel buffer from the producer's pool, holding `stored`.
    fn a_pixel_buffer_frame(
        &mut self,
        format: TextureFormat,
        (width, height): (u32, u32),
        stored: &[u8],
    ) -> ImageFrameDescriptorBody {
        let produced = self
            .producer_pool
            .acquire_the_next_output_pixel_buffer(
                width,
                height,
                format.host_view_pixel_format().expect("a host view"),
            )
            .expect("the producer's frame");
        produced
            .pixel_buffer
            .write_this_plane_from(0, stored)
            .expect("the producer writes its pixels");
        ImageFrameDescriptorBody {
            slot: produced
                .gpu_pool_slot_frame
                .expect("the producer's frame leases a claim-table row"),
            width,
            height,
            format: format as u32,
            colour: FRAME_COLOUR,
        }
    }

    fn convert(&mut self, image: &ImageFrameDescriptorBody) -> crate::core::Result<Vec<u8>> {
        let converted = self
            .stage
            .convert_into_the_next_pooled_pixel_buffer(&self.limited, image)?;
        assert_eq!(
            (converted.width, converted.height, converted.format),
            (image.width, image.height, TextureFormat::Rgba8Unorm as u32)
        );
        assert_eq!(converted.colour, image.colour);
        let converted_surface_id = self
            .gpu
            .published_frame_id_of_the_gpu_pool_slot_frame(&converted.slot)
            .expect("the converted frame names a slot of this runtime");
        let converted_pixel_buffer = self
            .gpu
            .get_pixel_buffer(&converted_surface_id)
            .expect("the converted frame's pixel buffer");
        let byte_count = (image.width * image.height * 4) as usize;
        Ok(unsafe {
            std::slice::from_raw_parts(converted_pixel_buffer.plane_base_address(0), byte_count)
        }
        .to_vec())
    }
}

/// A texture registered under a pool slot a claim table leases, as a producer's texture frame
/// is, with the leases that keep the frame live.
struct TextureFrameUnderAPoolSlot {
    image: ImageFrameDescriptorBody,
    pool_slot_key: &'static str,
    _row_lease: GpuPoolClaimTableRowLease,
    _claim_tables_of_the_pool: GpuPoolClaimTablesOfOnePool,
}

/// How a test texture frame is made.
struct TextureFrameToRegister<'pixels> {
    pool_slot_key: &'static str,
    texture_format: TextureFormat,
    texture_extent: (u32, u32),
    usage: TextureUsages,
    stored_pixels: Option<&'pixels [u8]>,
    format_the_frame_states: TextureFormat,
    extent_the_frame_states: (u32, u32),
    frame_generation: u64,
}

impl<'pixels> TextureFrameToRegister<'pixels> {
    fn written_with(
        pool_slot_key: &'static str,
        format: TextureFormat,
        extent: (u32, u32),
        stored_pixels: &'pixels [u8],
    ) -> Self {
        Self {
            pool_slot_key,
            texture_format: format,
            texture_extent: extent,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            stored_pixels: Some(stored_pixels),
            format_the_frame_states: format,
            extent_the_frame_states: extent,
            frame_generation: 1,
        }
    }
}

impl ConversionUnderTest {
    fn a_texture_frame(
        &self,
        to_register: TextureFrameToRegister<'_>,
    ) -> TextureFrameUnderAPoolSlot {
        let (width, height) = to_register.texture_extent;
        let texture = self
            .gpu
            .device()
            .create_texture_local(
                &TextureDescriptor::new(width, height, to_register.texture_format)
                    .with_usage(to_register.usage),
            )
            .expect("the frame's texture");
        let layout_it_was_left_in = match to_register.stored_pixels {
            None => VulkanLayout::UNDEFINED,
            Some(stored) => {
                let mut upload_pool = self
                    .gpu
                    .create_node_output_pixel_buffer_pool_for_a_native_node("an-upload");
                let upload = upload_pool
                    .acquire_the_next_output_pixel_buffer(
                        width,
                        height,
                        to_register
                            .texture_format
                            .host_view_pixel_format()
                            .expect("a host view"),
                    )
                    .expect("the upload's pixel buffer");
                upload
                    .pixel_buffer
                    .write_this_plane_from(0, stored)
                    .expect("the upload's pixels");
                let mut recorder = self
                    .gpu
                    .create_command_recorder("a-texture-frame-upload")
                    .expect("a recorder");
                recorder.begin().expect("begin");
                recorder
                    .record_buffer_barrier_before_a_transfer_read(&upload.pixel_buffer)
                    .expect("the buffer barrier");
                let resting_layout = recorder
                    .record_image_write_as_transfer_destination(
                        &texture,
                        VulkanLayout::UNDEFINED,
                        |recorder| {
                            recorder.record_copy_buffer_to_image(
                                &upload.pixel_buffer,
                                &texture,
                                VulkanLayout::TRANSFER_DST_OPTIMAL,
                                ImageCopyRegion::tightly_packed(width, height),
                            )
                        },
                    )
                    .expect("the upload");
                recorder.submit().expect("submit");
                recorder.wait_for_completion().expect("the upload runs");
                resting_layout
            }
        };
        self.gpu.register_texture_with_layout(
            to_register.pool_slot_key,
            texture,
            layout_it_was_left_in,
        );
        #[cfg(target_os = "linux")]
        self.live_service
            .state
            .register_surface(a_surface_share_registration_of(
                to_register.pool_slot_key,
                layout_it_was_left_in,
            ))
            .map_err(|_| ())
            .expect("the service takes the registration");
        let mut claim_tables_of_the_pool =
            GpuPoolClaimTablesOfOnePool::named(format!("{}-pool", to_register.pool_slot_key));
        let row_lease = claim_tables_of_the_pool
            .lease_a_row_for_a_slot(
                self.live_service.check_out_leases().gpu_pool_claim_tables(),
                to_register.pool_slot_key,
            )
            .expect("the pool leases its slot a row");
        row_lease
            .row()
            .publish_generation(to_register.frame_generation);
        self.live_service
            .check_out_leases()
            .publish_frame_generation(to_register.pool_slot_key, to_register.frame_generation)
            .expect("the lease table takes the frame");
        let (frame_width, frame_height) = to_register.extent_the_frame_states;
        TextureFrameUnderAPoolSlot {
            image: ImageFrameDescriptorBody {
                slot: GpuPoolSlotFrameDescriptorBody {
                    pool: row_lease.table().pool_uuid(),
                    slot: row_lease.row_index(),
                    generation: 1,
                    timeline_value: 0,
                    image_layout: 0,
                },
                width: frame_width,
                height: frame_height,
                format: to_register.format_the_frame_states as u32,
                colour: FRAME_COLOUR,
            },
            pool_slot_key: to_register.pool_slot_key,
            _row_lease: row_lease,
            _claim_tables_of_the_pool: claim_tables_of_the_pool,
        }
    }

    fn the_registered_layout_of(&self, pool_slot_key: &str) -> VulkanLayout {
        self.gpu
            .producer_registered_texture_for_surface_id(pool_slot_key)
            .expect("the texture is registered")
            .current_layout()
    }
}

/// What the surface-share service holds for a texture registered in this process, so the
/// layout the stage publishes lands somewhere a test can read.
#[cfg(target_os = "linux")]
fn a_surface_share_registration_of(
    pool_slot_key: &str,
    layout: VulkanLayout,
) -> crate::linux::surface_share::SurfaceRegistration<'_> {
    use crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::{
        VK_IMAGE_ALLOCATION_SIZE_DEFAULT, VK_IMAGE_ARRAY_LAYERS_DEFAULT,
        VK_IMAGE_MIP_LEVELS_DEFAULT, VK_IMAGE_SAMPLES_DEFAULT, VK_IMAGE_TILING_DEFAULT,
        VK_IMAGE_TYPE_DEFAULT, VK_IMAGE_USAGE_DEFAULT,
    };
    crate::linux::surface_share::SurfaceRegistration {
        surface_id: pool_slot_key,
        runtime_id: "a-producer",
        dma_buf_fds: Vec::new(),
        plane_sizes: Vec::new(),
        plane_offsets: Vec::new(),
        plane_strides: Vec::new(),
        width: 1,
        height: 1,
        format: "rgba8_unorm",
        resource_type: "texture",
        handle_type: "opaque_fd",
        drm_format_modifier: 0,
        produce_done_fd: None,
        consume_done_fd: None,
        current_image_layout: layout.0,
        vk_image_type: VK_IMAGE_TYPE_DEFAULT,
        vk_image_mip_levels: VK_IMAGE_MIP_LEVELS_DEFAULT,
        vk_image_array_layers: VK_IMAGE_ARRAY_LAYERS_DEFAULT,
        vk_image_samples: VK_IMAGE_SAMPLES_DEFAULT,
        vk_image_tiling: VK_IMAGE_TILING_DEFAULT,
        vk_image_usage: VK_IMAGE_USAGE_DEFAULT,
        vk_image_allocation_size: VK_IMAGE_ALLOCATION_SIZE_DEFAULT,
        vk_memory_type_index: None,
        exporting_device_uuid: None,
        tensor_layout: None,
        pixel_planes_within_one_allocation: None,
    }
}

/// A BGRA and an RGBA frame from a native node's output pool each convert into an
/// `rgba8_unorm` frame, every pixel's channels in RGBA order, at an extent no workgroup divides,
/// the frame keeping its colour; a frame of another extent after them converts as rightly.
///
/// GPU-gated: prints SKIPPED and passes when no device is present.
#[test]
fn pixel_buffer_frames_convert_into_rgba8_unorm_frames_in_rgba_order() {
    let Some(mut under_test) = a_conversion_under_test(
        "pixel_buffer_frames_convert_into_rgba8_unorm_frames_in_rgba_order",
    ) else {
        return;
    };
    for (format, extent) in [
        (TextureFormat::Bgra8Unorm, (34, 7)),
        (TextureFormat::Rgba8Unorm, (34, 7)),
        (TextureFormat::Bgra8Unorm, (19, 23)),
    ] {
        let stored = stored_pixels_of(extent.0, extent.1);
        let image = under_test.a_pixel_buffer_frame(format, extent, &stored);
        let rgba = under_test
            .convert(&image)
            .unwrap_or_else(|refusal| panic!("a {format:?} {extent:?} frame converts: {refusal}"));
        assert_eq!(
            rgba,
            in_rgba_order(&stored, format),
            "{format:?} {extent:?}"
        );
    }
}

/// A frame whose pixel buffer is larger than its stated extent packs tightly is refused, since
/// the copy would read its rows as tightly packed; so is a frame of no pixels.
///
/// GPU-gated: prints SKIPPED and passes when no device is present.
#[test]
fn a_frame_its_pixel_buffer_does_not_hold_tightly_packed_or_of_no_pixels_is_refused() {
    let Some(mut under_test) = a_conversion_under_test(
        "a_frame_its_pixel_buffer_does_not_hold_tightly_packed_or_of_no_pixels_is_refused",
    ) else {
        return;
    };
    let stored = stored_pixels_of(34, 7);
    let mut image = under_test.a_pixel_buffer_frame(TextureFormat::Bgra8Unorm, (34, 7), &stored);
    image.width = 30;
    let refusal = under_test
        .convert(&image)
        .expect_err("a 34-pixel-wide buffer holds no tightly packed 30-pixel-wide frame");
    assert!(matches!(refusal, Error::BufferError(_)), "{refusal}");
    assert!(refusal.to_string().contains("30x7"), "{refusal}");

    image.width = 0;
    let refusal = under_test
        .convert(&image)
        .expect_err("a frame of no pixels");
    assert!(refusal.to_string().contains("0x7"), "{refusal}");
}

/// A BGRA and an RGBA texture registered under a pool slot each convert pixel for pixel; the
/// texture's registration and the surface-share service both end with it in
/// SHADER_READ_ONLY_OPTIMAL, the layout the kernel sampled it in.
///
/// GPU-gated: prints SKIPPED and passes when no device is present.
#[test]
fn texture_frames_convert_and_leave_their_texture_in_shader_read_only_optimal() {
    let Some(mut under_test) = a_conversion_under_test(
        "texture_frames_convert_and_leave_their_texture_in_shader_read_only_optimal",
    ) else {
        return;
    };
    for (pool_slot_key, format) in [
        ("a-bgra-texture-slot", TextureFormat::Bgra8Unorm),
        ("an-rgba-texture-slot", TextureFormat::Rgba8Unorm),
    ] {
        let extent = (21, 17);
        let stored = stored_pixels_of(extent.0, extent.1);
        let frame = under_test.a_texture_frame(TextureFrameToRegister::written_with(
            pool_slot_key,
            format,
            extent,
            &stored,
        ));
        assert_ne!(
            under_test.the_registered_layout_of(frame.pool_slot_key),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            "the upload leaves the texture in another layout"
        );

        let rgba = under_test
            .convert(&frame.image)
            .unwrap_or_else(|refusal| panic!("a {format:?} texture frame converts: {refusal}"));

        assert_eq!(rgba, in_rgba_order(&stored, format), "{format:?}");
        assert_eq!(
            under_test.the_registered_layout_of(frame.pool_slot_key),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            under_test
                .live_service
                .state
                .get_surface_planes(frame.pool_slot_key)
                .expect("the service holds the registration")
                .current_image_layout,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0
        );
    }
}

/// A texture frame is refused, its texture's layout untouched, when its texture was never
/// written, is of a format or extent other than its Image frame states, cannot be sampled, or
/// its frame is retired.
///
/// GPU-gated: prints SKIPPED and passes when no device is present.
#[test]
fn a_texture_frame_the_kernel_cannot_sample_as_stated_is_refused_naming_why() {
    let Some(mut under_test) = a_conversion_under_test(
        "a_texture_frame_the_kernel_cannot_sample_as_stated_is_refused_naming_why",
    ) else {
        return;
    };
    let extent = (16, 8);
    let stored = stored_pixels_of(extent.0, extent.1);
    let refusals: [(TextureFrameToRegister<'_>, &str); 5] = [
        (
            TextureFrameToRegister {
                stored_pixels: None,
                ..TextureFrameToRegister::written_with(
                    "a-never-written-slot",
                    TextureFormat::Bgra8Unorm,
                    extent,
                    &stored,
                )
            },
            "never been written",
        ),
        (
            TextureFrameToRegister {
                format_the_frame_states: TextureFormat::Bgra8Unorm,
                ..TextureFrameToRegister::written_with(
                    "an-rgba-slot-stated-bgra",
                    TextureFormat::Rgba8Unorm,
                    extent,
                    &stored,
                )
            },
            "is a rgba8_unorm texture, and its Image frame states bgra8_unorm",
        ),
        (
            TextureFrameToRegister {
                extent_the_frame_states: (8, 8),
                ..TextureFrameToRegister::written_with(
                    "a-slot-stated-another-extent",
                    TextureFormat::Bgra8Unorm,
                    extent,
                    &stored,
                )
            },
            "is a 16x8 texture, and its Image frame states 8x8",
        ),
        (
            TextureFrameToRegister {
                usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_DST,
                ..TextureFrameToRegister::written_with(
                    "an-unsampleable-slot",
                    TextureFormat::Bgra8Unorm,
                    extent,
                    &stored,
                )
            },
            "without \"texture_binding\" usage",
        ),
        (
            TextureFrameToRegister {
                frame_generation: 2,
                ..TextureFrameToRegister::written_with(
                    "a-retired-frame-slot",
                    TextureFormat::Bgra8Unorm,
                    extent,
                    &stored,
                )
            },
            "a-retired-frame-slot",
        ),
    ];
    for (to_register, named) in refusals {
        let frame = under_test.a_texture_frame(to_register);
        let layout_before = under_test.the_registered_layout_of(frame.pool_slot_key);

        let refusal = under_test
            .convert(&frame.image)
            .expect_err("the kernel cannot sample the frame as stated");

        assert!(refusal.to_string().contains(named), "{named}: {refusal}");
        assert_eq!(
            under_test.the_registered_layout_of(frame.pool_slot_key),
            layout_before,
            "{named}"
        );
    }
}
