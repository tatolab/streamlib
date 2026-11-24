// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// An Array frame of CPU attributes reads back as a mapping of name to a
/// tensor borrowed in the sample, each stating its dtype and shape; a
/// write missing a declared name is refused naming it.
#[test]
fn an_array_frame_of_cpu_attributes_reads_back_as_borrowed_tensors_by_name() {
    Python::initialize();
    Python::attach(|python| {
        put_the_stream_distribution_source_on_sys_path(python);
        let (source, destination) = one_wired_link(python, "array-frame");
        let no_gpu = PythonGpuContextLimitedAccess::new_for_helper_process(None);
        let attributes = python
            .eval(
                c"{'boxes': memoryview(bytes(range(24))).cast('f', [2, 3]), \
                   'labels': memoryview(bytes([7, 8, 9]))}",
                None,
                None,
            )
            .unwrap();
        source
            .write_array_frame_to_output_port(
                python,
                "frames_to_downstream",
                &attributes,
                &["boxes".to_string()],
                None,
                &no_gpu,
                Some(42),
                true,
            )
            .unwrap();

        let received = read_as_the_family_type(python, &destination, PortFamilyOfTheRead::Array);
        assert_eq!(received.len().unwrap(), 2);
        assert_eq!(
            received
                .getattr("timestamp_ns")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            42
        );
        for (name, dtype, shape, expected_bytes) in [
            (
                "boxes",
                "float32",
                vec![2u64, 3],
                (0u8..24).collect::<Vec<_>>(),
            ),
            ("labels", "uint8", vec![3], vec![7, 8, 9]),
        ] {
            let attribute = received.get_item(name).unwrap();
            let attribute = attribute
                .cast::<PythonTensorCpuAttribute>()
                .expect("a CPU attribute reads as a TensorCpuAttribute");
            assert_eq!(attribute.get().dtype(), dtype);
            assert_eq!(attribute.get().shape(), shape);
            assert_eq!(
                attribute.get().attribute_bytes(),
                &expected_bytes[..],
                "{name}"
            );
        }

        source
            .write_array_frame_to_output_port(
                python,
                "frames_to_downstream",
                &received,
                &["boxes".to_string()],
                None,
                &no_gpu,
                Some(43),
                true,
            )
            .expect("a read Array frame relays unchanged");
        let relayed = read_as_the_family_type(python, &destination, PortFamilyOfTheRead::Array);
        let relayed_boxes = relayed.get_item("boxes").unwrap();
        let relayed_boxes = relayed_boxes.cast::<PythonTensorCpuAttribute>().unwrap();
        assert_eq!(relayed_boxes.get().shape(), vec![2u64, 3]);
        assert_eq!(
            relayed_boxes.get().attribute_bytes(),
            &(0u8..24).collect::<Vec<_>>()[..]
        );

        let residency_refusal = source
            .write_array_frame_to_output_port(
                python,
                "frames_to_downstream",
                &attributes,
                &[],
                Some(ArrayOutputPortDeclaredResidency::Gpu),
                &no_gpu,
                None,
                true,
            )
            .expect_err("a CPU attribute is refused on a GPU-resident port");
        assert!(
            residency_refusal.to_string().contains("residency=\"gpu\""),
            "{residency_refusal}"
        );

        let missing_boxes = python
            .eval(c"{'labels': memoryview(bytes([1]))}", None, None)
            .unwrap();
        let refusal = source
            .write_array_frame_to_output_port(
                python,
                "frames_to_downstream",
                &missing_boxes,
                &["boxes".to_string()],
                None,
                &no_gpu,
                None,
                true,
            )
            .expect_err("`boxes` is declared");
        assert!(refusal.to_string().contains("\"boxes\""), "{refusal}");

        let float64 = python
            .eval(c"{'boxes': memoryview(bytes(8)).cast('d')}", None, None)
            .unwrap();
        let refusal = source
            .write_array_frame_to_output_port(
                python,
                "frames_to_downstream",
                &float64,
                &[],
                None,
                &no_gpu,
                None,
                true,
            )
            .expect_err("float64 is no tensor element type");
        assert!(refusal.to_string().contains("float32"), "{refusal}");
    });
}

/// An Image frame over a claim-tabled pool slot reads back as an
/// `Texture` naming the slot's published frame, its size, format and
/// colour, holding the slot while it lives; written on, it publishes the
/// same frame again. Needs a Vulkan device; skips where none comes up.
#[cfg(target_os = "linux")]
#[test]
fn an_image_frame_reads_back_claimed_and_writes_on_as_the_same_frame() {
    use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
    use streamlib::sdk::context::{GpuPoolClaimTablesOfOnePool, SurfaceStore};
    use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
    use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};

    const POOL_SLOT_KEY: &str = "image-frame-round-trip-slot";
    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let exported = match HostVulkanBuffer::new_opaque_fd_export(&host_device, 64 * 4 * 16) {
        Ok(exported) => exported,
        Err(unavailable) => {
            tracing::warn!("skipping: no OPAQUE_FD buffer export on this device ({unavailable})");
            return;
        }
    };
    let pool_slot =
        PixelBuffer::from_host_vulkan_buffer(Arc::new(exported), 64, 16, 4, PixelFormat::Bgra32);
    let share = SurfaceShareUnderTest::start("image-frame-round-trip");
    let engine_surface_store = SurfaceStore::new(
        share.socket_path.to_string_lossy().into_owned(),
        "engine:image-frame-round-trip".to_string(),
    );
    engine_surface_store
        .connect()
        .expect("the engine's store connects");
    engine_surface_store
        .register_buffer(POOL_SLOT_KEY, &pool_slot)
        .expect("the engine registers the pool slot");
    let mut claim_tables_of_the_pool = GpuPoolClaimTablesOfOnePool::named("image-frame-round-trip");
    let row_lease = claim_tables_of_the_pool
        .lease_a_row_for_a_slot(
            share.check_out_leases().gpu_pool_claim_tables(),
            POOL_SLOT_KEY,
        )
        .expect("the pool leases its slot a row");
    row_lease.row().publish_generation(1);
    share.publish_frame_generation_over_a_registered_pool_slot(POOL_SLOT_KEY, 1);
    let exchange_client = share.exchange_client_labelled_importing_on(
        "image-frame-round-trip",
        Ok(host_device.physical_device_uuid()),
    );

    Python::initialize();
    Python::attach(|python| {
        put_the_stream_distribution_source_on_sys_path(python);
        lend_the_typed_read_gpu_offer_to_this_interpreter(python);
        let gpu = Bound::new(
            python,
            PythonGpuContextLimitedAccess::new_for_helper_process(Some(exchange_client)),
        )
        .unwrap();
        let image_frame = ImageFrameDescriptorBody {
            slot: GpuPoolSlotFrameDescriptorBody {
                pool: row_lease.table().pool_uuid(),
                slot: row_lease.row_index(),
                generation: 1,
                timeline_value: 0,
                image_layout: 0,
            },
            width: 64,
            height: 16,
            format: TextureFormat::Bgra8Unorm as u32,
            colour: ImageFrameH273Colour {
                primaries: 1,
                transfer: 13,
                matrix: 1,
                range: 1,
            },
        };
        let read_image_frame_on = |destination: &PythonNodeLinkDataAccess| {
            destination
                .read_from_input_port_offering_gpu_access(
                    python,
                    "frames_from_upstream",
                    PortPayloadReadShape::FamilyType(PortFamiliesOfTheRead::of_one(
                        PortFamilyOfTheRead::Image,
                    )),
                    Some(&gpu),
                )
                .unwrap()
                .expect("the wired input received nothing")
        };
        let (source, destination) = one_wired_link(python, "image-frame-read");
        source
            .write_described_by(
                python,
                "frames_to_downstream",
                PyBytes::new(python, &[]).as_any(),
                Some(7),
                true,
                |_| Ok(DataChannelPayloadDescriptor::image_frame(image_frame)),
            )
            .unwrap();
        let frame = read_image_frame_on(&destination);
        let field = |name: &str| frame.getattr(name).unwrap();
        assert_eq!(
            field("surface_id").extract::<String>().unwrap(),
            format!("{POOL_SLOT_KEY}#1")
        );
        assert_eq!(field("width").extract::<u32>().unwrap(), 64);
        assert_eq!(field("height").extract::<u32>().unwrap(), 16);
        assert_eq!(field("format").extract::<String>().unwrap(), "bgra8_unorm");
        assert_eq!(field("timestamp_ns").extract::<i64>().unwrap(), 7);
        let colour = field("colour");
        for (axis, name) in [
            ("primaries", "bt709"),
            ("transfer", "srgb"),
            ("matrix", "bt709"),
            ("range", "full"),
        ] {
            assert_eq!(
                colour.getattr(axis).unwrap().extract::<String>().unwrap(),
                name
            );
        }
        assert!(row_lease.row().is_claimed(), "a read frame holds its slot");

        let (source_on, destination_on) = one_wired_link(python, "image-frame-write-on");
        let format_refusal = source_on
            .write_image_frame_to_output_port(
                python,
                "frames_to_downstream",
                &frame,
                &["nv12".to_string()],
                gpu.get(),
                None,
                true,
            )
            .expect_err("the frame's format is not the one the port declares");
        assert!(
            format_refusal.to_string().contains("formats="),
            "{format_refusal}"
        );
        source_on
            .write_image_frame_to_output_port(
                python,
                "frames_to_downstream",
                &frame,
                &[frame
                    .getattr("format")
                    .unwrap()
                    .extract::<String>()
                    .unwrap()],
                gpu.get(),
                None,
                true,
            )
            .unwrap();
        let frame_written_on = read_image_frame_on(&destination_on);
        for name in ["surface_id", "width", "height", "format", "colour"] {
            assert!(
                frame_written_on
                    .getattr(name)
                    .unwrap()
                    .eq(frame.getattr(name).unwrap())
                    .unwrap(),
                "{name} survives the write on"
            );
        }
        drop((frame, frame_written_on));
        python.run(c"import gc; gc.collect()", None, None).unwrap();
        withdraw_the_lent_typed_read_gpu_offer(python);
    });
    assert!(
        !row_lease.row().is_claimed(),
        "a dropped frame lets its slot go"
    );
}

/// `TextureToTensor` borrows an Image frame's pool slot as its one Array
/// attribute: the attribute a reader claims exports the very address the
/// Image frame itself does, the slot's mapping, with no copy between.
/// Needs a Vulkan device; skips where none comes up.
#[cfg(target_os = "linux")]
#[test]
fn texture_to_tensor_borrows_the_frames_own_slot_with_no_copy() {
    use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
    use streamlib::sdk::context::{GpuPoolClaimTablesOfOnePool, SurfaceStore};
    use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
    use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};
    use streamlib_media_builtins::pin_type_conversion::texture_to_tensor::array_frame_of_the_image_frame;

    const POOL_SLOT_KEY: &str = "image-to-array-slot";
    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let exported = match HostVulkanBuffer::new_opaque_fd_export(&host_device, 64 * 4 * 16) {
        Ok(exported) => exported,
        Err(unavailable) => {
            tracing::warn!("skipping: no OPAQUE_FD buffer export on this device ({unavailable})");
            return;
        }
    };
    let pool_slot =
        PixelBuffer::from_host_vulkan_buffer(Arc::new(exported), 64, 16, 4, PixelFormat::Bgra32);
    let share = SurfaceShareUnderTest::start("image-to-array");
    let engine_surface_store = SurfaceStore::new(
        share.socket_path.to_string_lossy().into_owned(),
        "engine:image-to-array".to_string(),
    );
    engine_surface_store
        .connect()
        .expect("the engine's store connects");
    engine_surface_store
        .register_buffer(POOL_SLOT_KEY, &pool_slot)
        .expect("the engine registers the pool slot");
    let mut claim_tables_of_the_pool = GpuPoolClaimTablesOfOnePool::named("image-to-array");
    let row_lease = claim_tables_of_the_pool
        .lease_a_row_for_a_slot(
            share.check_out_leases().gpu_pool_claim_tables(),
            POOL_SLOT_KEY,
        )
        .expect("the pool leases its slot a row");
    row_lease.row().publish_generation(1);
    share.publish_frame_generation_over_a_registered_pool_slot(POOL_SLOT_KEY, 1);
    let exchange_client = share.exchange_client_labelled_importing_on(
        "image-to-array",
        Ok(host_device.physical_device_uuid()),
    );

    Python::initialize();
    Python::attach(|python| {
        put_the_stream_distribution_source_on_sys_path(python);
        lend_the_typed_read_gpu_offer_to_this_interpreter(python);
        let gpu = Bound::new(
            python,
            PythonGpuContextLimitedAccess::new_for_helper_process(Some(exchange_client)),
        )
        .unwrap();
        let image_frame = DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
            slot: GpuPoolSlotFrameDescriptorBody {
                pool: row_lease.table().pool_uuid(),
                slot: row_lease.row_index(),
                generation: 1,
                timeline_value: 0,
                image_layout: 0,
            },
            width: 64,
            height: 16,
            format: TextureFormat::Bgra8Unorm as u32,
            colour: ImageFrameH273Colour {
                primaries: 1,
                transfer: 13,
                matrix: 1,
                range: 1,
            },
        });
        let (array_descriptor, array_payload) =
            array_frame_of_the_image_frame(image_frame).expect("a bgra8 frame converts");
        let data_address_of = |tensor: &Bound<'_, PyAny>| {
            let capsule = tensor.call_method0("__dlpack__").unwrap();
            // SAFETY: an unconsumed `__dlpack__` capsule carries a
            // `ManagedTensor` under the name `dltensor`.
            unsafe {
                let managed_tensor =
                    pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), c"dltensor".as_ptr())
                        as *const streamlib_adapter_cuda::dlpack::ManagedTensor;
                (*managed_tensor).dl_tensor.data as usize
            }
        };

        let (image_source, image_destination) = one_wired_link(python, "image-to-array-image");
        image_source
            .write_described_by(
                python,
                "frames_to_downstream",
                PyBytes::new(python, &[]).as_any(),
                Some(7),
                true,
                |_| Ok(image_frame),
            )
            .unwrap();
        let read_image = image_destination
            .read_from_input_port_offering_gpu_access(
                python,
                "frames_from_upstream",
                PortPayloadReadShape::FamilyType(PortFamiliesOfTheRead::of_one(
                    PortFamilyOfTheRead::Image,
                )),
                Some(&gpu),
            )
            .unwrap()
            .expect("the image link received nothing");
        let image_data_address = data_address_of(&read_image);

        let (array_source, array_destination) = one_wired_link(python, "image-to-array-array");
        array_source
            .write_described_by(
                python,
                "frames_to_downstream",
                PyBytes::new(python, &array_payload).as_any(),
                Some(7),
                true,
                |_| Ok(array_descriptor),
            )
            .unwrap();
        let read_array = array_destination
            .read_from_input_port_offering_gpu_access(
                python,
                "frames_from_upstream",
                PortPayloadReadShape::FamilyType(PortFamiliesOfTheRead::of_one(
                    PortFamilyOfTheRead::Array,
                )),
                Some(&gpu),
            )
            .unwrap()
            .expect("the array link received nothing");
        let image_attribute = read_array.get_item("image").unwrap();
        assert_eq!(
            image_attribute
                .getattr("dtype")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "uint8"
        );
        assert_eq!(
            image_attribute
                .getattr("shape")
                .unwrap()
                .extract::<Vec<u64>>()
                .unwrap(),
            vec![16, 64, 4]
        );
        let array_data_address = data_address_of(&image_attribute);

        assert_ne!(image_data_address, 0);
        assert_eq!(
            array_data_address, image_data_address,
            "the attribute is the frame's slot mapping, not a copy of it"
        );
        assert!(
            row_lease.row().is_claimed(),
            "the read attribute holds its slot"
        );
        drop((read_image, image_attribute, read_array));
        python.run(c"import gc; gc.collect()", None, None).unwrap();
        withdraw_the_lent_typed_read_gpu_offer(python);
    });
    assert!(
        !row_lease.row().is_claimed(),
        "dropped reads let the slot go"
    );
}

/// `TensorToTexture` copies a CPU `[h, w, 4]` attribute once into a pixel
/// buffer of its output pool, and the Image frame it describes reads back
/// as exactly those pixels, claimed from that buffer's slot.
/// Needs a Vulkan device; skips where none comes up.
#[cfg(target_os = "linux")]
#[test]
fn tensor_to_texture_copies_a_cpu_attribute_into_a_pixel_buffer_read_back_as_the_image() {
    use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
    use streamlib::sdk::context::{
        GpuPoolClaimTablesOfOnePool, NativeNodeOutputPixelBufferFrame, SurfaceStore,
    };
    use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
    use streamlib::sdk::iceoryx2::{
        ArrayFrameCpuAttributeToPack, array_frame_payload_of_cpu_attributes,
    };
    use streamlib::sdk::rhi::{
        PixelBuffer, PixelBufferPoolSlotId, PixelFormat, PublishedPixelBufferFrameId,
    };
    use streamlib_media_builtins::pin_type_conversion::tensor_to_texture::image_frame_of_the_array_frame;

    const POOL_SLOT_KEY: &str = "array-to-image-slot";
    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 16;
    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let exported =
        match HostVulkanBuffer::new_opaque_fd_export(&host_device, u64::from(WIDTH * 4 * HEIGHT)) {
            Ok(exported) => exported,
            Err(unavailable) => {
                tracing::warn!(
                    "skipping: no OPAQUE_FD buffer export on this device ({unavailable})"
                );
                return;
            }
        };
    let pool_slot = PixelBuffer::from_host_vulkan_buffer(
        Arc::new(exported),
        WIDTH,
        HEIGHT,
        4,
        PixelFormat::Rgba32,
    );
    let share = SurfaceShareUnderTest::start("array-to-image");
    let engine_surface_store = SurfaceStore::new(
        share.socket_path.to_string_lossy().into_owned(),
        "engine:array-to-image".to_string(),
    );
    engine_surface_store
        .connect()
        .expect("the engine's store connects");
    engine_surface_store
        .register_buffer(POOL_SLOT_KEY, &pool_slot)
        .expect("the engine registers the pool slot");
    let mut claim_tables_of_the_pool = GpuPoolClaimTablesOfOnePool::named("array-to-image");
    let row_lease = claim_tables_of_the_pool
        .lease_a_row_for_a_slot(
            share.check_out_leases().gpu_pool_claim_tables(),
            POOL_SLOT_KEY,
        )
        .expect("the pool leases its slot a row");
    row_lease.row().publish_generation(1);
    share.publish_frame_generation_over_a_registered_pool_slot(POOL_SLOT_KEY, 1);
    let exchange_client = share.exchange_client_labelled_importing_on(
        "array-to-image",
        Ok(host_device.physical_device_uuid()),
    );

    let pixels: Vec<u8> = (0..WIDTH * HEIGHT * 4)
        .map(|byte_index| (byte_index * 7 % 251) as u8)
        .collect();
    let (array_body, array_payload) =
        array_frame_payload_of_cpu_attributes(&[ArrayFrameCpuAttributeToPack {
            name: "image",
            dtype: "uint8",
            shape: &[u64::from(HEIGHT), u64::from(WIDTH), 4],
            data: &pixels,
        }])
        .expect("the attribute packs");
    let mut pixel_buffers_acquired = 0;
    let image_frame = image_frame_of_the_array_frame(
        "image",
        TextureFormat::Rgba8Unorm,
        DataChannelPayloadDescriptor::array_frame(array_body),
        &array_payload,
        |width, height, pixel_format| {
            pixel_buffers_acquired += 1;
            assert_eq!(
                (width, height, pixel_format),
                (WIDTH, HEIGHT, PixelFormat::Rgba32)
            );
            Ok(NativeNodeOutputPixelBufferFrame {
                published_frame_id: PublishedPixelBufferFrameId::new(
                    PixelBufferPoolSlotId::from_str(POOL_SLOT_KEY),
                    1,
                ),
                pixel_buffer: pool_slot.clone(),
                gpu_pool_slot_frame: Some(GpuPoolSlotFrameDescriptorBody {
                    pool: row_lease.table().pool_uuid(),
                    slot: row_lease.row_index(),
                    generation: 1,
                    timeline_value: 0,
                    image_layout: 0,
                }),
            })
        },
    )
    .expect("a CPU [h, w, 4] uint8 attribute converts");
    assert_eq!(pixel_buffers_acquired, 1);

    Python::initialize();
    Python::attach(|python| {
        put_the_stream_distribution_source_on_sys_path(python);
        lend_the_typed_read_gpu_offer_to_this_interpreter(python);
        let gpu = Bound::new(
            python,
            PythonGpuContextLimitedAccess::new_for_helper_process(Some(exchange_client)),
        )
        .unwrap();
        let (source, destination) = one_wired_link(python, "array-to-image");
        source
            .write_described_by(
                python,
                "frames_to_downstream",
                PyBytes::new(python, &[]).as_any(),
                Some(7),
                true,
                |_| Ok(image_frame),
            )
            .unwrap();
        let read_image = destination
            .read_from_input_port_offering_gpu_access(
                python,
                "frames_from_upstream",
                PortPayloadReadShape::FamilyType(PortFamiliesOfTheRead::of_one(
                    PortFamilyOfTheRead::Image,
                )),
                Some(&gpu),
            )
            .unwrap()
            .expect("the image link received nothing");
        let field = |name: &str| read_image.getattr(name).unwrap();
        assert_eq!(
            field("surface_id").extract::<String>().unwrap(),
            format!("{POOL_SLOT_KEY}#1")
        );
        assert_eq!(field("width").extract::<u32>().unwrap(), WIDTH);
        assert_eq!(field("height").extract::<u32>().unwrap(), HEIGHT);
        assert_eq!(field("format").extract::<String>().unwrap(), "rgba8_unorm");
        let capsule = read_image.call_method0("__dlpack__").unwrap();
        // SAFETY: an unconsumed `__dlpack__` capsule carries a
        // `ManagedTensor` under the name `dltensor`, whose data is the
        // frame's host-mapped pixels while the capsule lives.
        let pixels_read_back = unsafe {
            let managed_tensor =
                pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), c"dltensor".as_ptr())
                    as *const streamlib_adapter_cuda::dlpack::ManagedTensor;
            std::slice::from_raw_parts((*managed_tensor).dl_tensor.data as *const u8, pixels.len())
                .to_vec()
        };
        assert!(
            pixels_read_back == pixels,
            "the Image frame reads back the attribute's pixels"
        );
        assert!(
            row_lease.row().is_claimed(),
            "the read frame holds its slot"
        );
        drop((capsule, read_image));
        python.run(c"import gc; gc.collect()", None, None).unwrap();
        withdraw_the_lent_typed_read_gpu_offer(python);
    });
    assert!(
        !row_lease.row().is_claimed(),
        "a dropped read lets the slot go"
    );
}
