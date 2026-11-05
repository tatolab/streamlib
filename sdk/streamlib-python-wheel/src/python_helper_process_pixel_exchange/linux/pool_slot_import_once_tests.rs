// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use streamlib::sdk::context::SurfaceStore;
use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

const PIXEL_BUFFER_WIDTH: u32 = 64;
const PIXEL_BUFFER_HEIGHT: u32 = 16;
const PIXEL_BUFFER_BYTE_SIZE: u64 = 64 * 4 * 16;

/// A two-slot pool, each slot an engine-exported OPAQUE_FD pixel buffer
/// stamped with its own byte; `None` where no Vulkan device or export comes
/// up.
fn two_stamped_pool_slots(host_device: &Arc<HostVulkanDevice>) -> Option<[(PixelBuffer, u8); 2]> {
    let stamped_slot = |stamp: u8| -> Option<(PixelBuffer, u8)> {
        let exported =
            match HostVulkanBuffer::new_opaque_fd_export(host_device, PIXEL_BUFFER_BYTE_SIZE) {
                Ok(exported) => exported,
                Err(unavailable) => {
                    tracing::warn!(
                        "skipping: no OPAQUE_FD buffer export on this device ({unavailable})"
                    );
                    return None;
                }
            };
        // SAFETY: the export is a live mapping of PIXEL_BUFFER_BYTE_SIZE bytes.
        unsafe {
            std::ptr::write_bytes(
                exported.mapped_ptr(),
                stamp,
                PIXEL_BUFFER_BYTE_SIZE as usize,
            )
        };
        Some((
            PixelBuffer::from_host_vulkan_buffer(
                Arc::new(exported),
                PIXEL_BUFFER_WIDTH,
                PIXEL_BUFFER_HEIGHT,
                4,
                PixelFormat::Bgra32,
            ),
            stamp,
        ))
    };
    Some([stamped_slot(0x3C)?, stamped_slot(0xC3)?])
}

fn pixel_buffer_import_of(
    checked_out: &HelperCheckedOutSurface,
) -> &Arc<HelperPixelBufferPoolSlotImport> {
    let HelperCheckedOutSurface::PixelBuffer(pixel_surface) = checked_out else {
        panic!("a pixel_buffer registration must check out as a pixel buffer");
    };
    &pixel_surface.pixel_buffer_pool_slot_import
}

fn every_byte_of(checked_out: &HelperCheckedOutSurface) -> &[u8] {
    let HelperCheckedOutSurface::PixelBuffer(pixel_surface) = checked_out else {
        panic!("a pixel_buffer registration must check out as a pixel buffer");
    };
    let base_address = pixel_surface.host_mapped_base_address();
    assert!(!base_address.is_null());
    // SAFETY: the mapping spans the slot's PIXEL_BUFFER_BYTE_SIZE bytes and
    // lives as long as `checked_out`.
    unsafe { std::slice::from_raw_parts(base_address, PIXEL_BUFFER_BYTE_SIZE as usize) }
}

/// Five frames over a two-slot pool import each slot once and read the
/// slot's own bytes every frame; a retired slot leaves the cache on the
/// connection's next reply, and a replaced connection empties it. Needs a
/// Vulkan device; skips where none comes up.
#[test]
fn five_frames_over_a_two_slot_pool_import_each_slot_once() {
    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let Some(pool_slots) = two_stamped_pool_slots(&host_device) else {
        return;
    };
    let share = SurfaceShareUnderTest::start("pool-slot-import-once");
    let engine_surface_store = SurfaceStore::new(
        share.socket_path.to_string_lossy().into_owned(),
        "engine:pool-slot-import-once-under-test".to_string(),
    );
    engine_surface_store
        .connect()
        .expect("the engine's store connects");
    let slot_ids = ["import-once-slot-a", "import-once-slot-b"];
    for (slot_id, (pixel_buffer, _)) in slot_ids.iter().zip(&pool_slots) {
        engine_surface_store
            .register_buffer(slot_id, pixel_buffer)
            .expect("the engine registers the pool slot");
    }
    let exchange_client = share.exchange_client_labelled_importing_on(
        "pool-slot-import-once-under-test",
        Ok(host_device.physical_device_uuid()),
    );

    let mut first_import_of_each_slot: [Option<Arc<HelperPixelBufferPoolSlotImport>>; 2] =
        [None, None];
    for frame_index in 0..5u64 {
        let slot_index = (frame_index % 2) as usize;
        let frame_generation = frame_index / 2 + 1;
        share.publish_frame_generation_over_a_registered_pool_slot(
            slot_ids[slot_index],
            frame_generation,
        );
        let frame_id = format!("{}#{frame_generation}", slot_ids[slot_index]);
        let checked_out = exchange_client
            .check_out_and_import(&frame_id)
            .unwrap_or_else(|refusal| panic!("frame {frame_id} checks out: {refusal}"));
        let stamp = pool_slots[slot_index].1;
        assert!(
            every_byte_of(&checked_out)
                .iter()
                .all(|byte| *byte == stamp),
            "frame {frame_id} must read its own slot's bytes"
        );
        let import = pixel_buffer_import_of(&checked_out);
        match &first_import_of_each_slot[slot_index] {
            Some(first_import) => assert!(
                Arc::ptr_eq(first_import, import),
                "frame {frame_id} must reuse its slot's first import"
            ),
            None => first_import_of_each_slot[slot_index] = Some(Arc::clone(import)),
        }
    }
    let [Some(slot_a_import), Some(slot_b_import)] = &first_import_of_each_slot else {
        panic!("both slots carried a frame");
    };
    assert!(!Arc::ptr_eq(slot_a_import, slot_b_import));
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        2,
        "five frames over two slots import twice"
    );

    engine_surface_store
        .release(slot_ids[1])
        .expect("the engine unregisters slot b");
    share.publish_frame_generation_over_a_registered_pool_slot(slot_ids[0], 4);
    let after_the_retirement = exchange_client
        .check_out_and_import(&format!("{}#4", slot_ids[0]))
        .expect("slot a still checks out");
    assert!(Arc::ptr_eq(
        slot_a_import,
        pixel_buffer_import_of(&after_the_retirement)
    ));
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        1,
        "the retired slot leaves the cache on the connection's next reply"
    );
    drop(after_the_retirement);

    drop(exchange_client.surface_share_connection.lock().take());
    exchange_client
        .surface_share_request(&serde_json::json!({"op": "lookup", "surface_id": "absent"}))
        .expect("a new connection answers");
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        0,
        "no import outlives the connection the service would retire it on"
    );
}

/// Check out `frames` published frames round-robin over `slot_ids` and keep
/// each slot's first import, asserting every later frame over the slot reads
/// through that same import.
fn check_out_frames_round_robin<SlotImport>(
    share: &SurfaceShareUnderTest,
    exchange_client: &Arc<HelperProcessGpuExchangeClient>,
    slot_ids: &[&str],
    frames: u64,
    pool_slot_import_of: impl Fn(&HelperCheckedOutSurface) -> Arc<SlotImport>,
) -> Vec<Arc<SlotImport>> {
    let mut first_import_of_each_slot: Vec<Option<Arc<SlotImport>>> =
        slot_ids.iter().map(|_| None).collect();
    for frame_index in 0..frames {
        let slot_index = (frame_index as usize) % slot_ids.len();
        let frame_generation = frame_index / slot_ids.len() as u64 + 1;
        share.publish_frame_generation_over_a_registered_pool_slot(
            slot_ids[slot_index],
            frame_generation,
        );
        let frame_id = format!("{}#{frame_generation}", slot_ids[slot_index]);
        let checked_out = exchange_client
            .check_out_and_import(&frame_id)
            .unwrap_or_else(|refusal| panic!("frame {frame_id} checks out: {refusal}"));
        let import = pool_slot_import_of(&checked_out);
        match &first_import_of_each_slot[slot_index] {
            Some(first_import) => assert!(
                Arc::ptr_eq(first_import, &import),
                "frame {frame_id} must reuse its slot's first import"
            ),
            None => first_import_of_each_slot[slot_index] = Some(import),
        }
    }
    first_import_of_each_slot
        .into_iter()
        .map(|import| import.expect("every slot carried a frame"))
        .collect()
}

/// Five frames over a two-slot pool of OPAQUE_FD textures import each slot's
/// image once, and each frame still runs its own layout acquire and release.
/// Needs a device that exports an OPAQUE_FD image; skips where none does.
#[test]
fn five_texture_frames_over_a_two_slot_pool_import_each_slot_once() {
    use streamlib::sdk::engine::host_rhi::HostVulkanTexture;
    use streamlib::sdk::engine::{HostSurfaceStoreExt as _, HostTextureExt as _};
    use streamlib::sdk::rhi::{
        Texture, TextureDescriptor, TextureFormat, TextureUsages, VulkanLayout,
    };

    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let descriptor = TextureDescriptor::new(64, 16, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC);
    let mut textures = Vec::new();
    for _ in 0..2 {
        match HostVulkanTexture::new_opaque_fd_export(&host_device, &descriptor) {
            Ok(exported) => textures.push(Texture::from_vulkan(exported)),
            Err(unavailable) => {
                tracing::warn!(
                    "skipping: no OPAQUE_FD image export on this device ({unavailable})"
                );
                return;
            }
        }
    }
    let share = SurfaceShareUnderTest::start("texture-import-once");
    let engine_surface_store = SurfaceStore::new(
        share.socket_path.to_string_lossy().into_owned(),
        "engine:texture-import-once-under-test".to_string(),
    );
    engine_surface_store
        .connect()
        .expect("the engine's store connects");
    let slot_ids = ["texture-import-once-slot-a", "texture-import-once-slot-b"];
    for (slot_id, texture) in slot_ids.iter().zip(&textures) {
        engine_surface_store
            .register_texture(slot_id, texture, None, None, VulkanLayout::UNDEFINED)
            .expect("the engine registers the texture pool slot");
    }
    let exchange_client = share.exchange_client_labelled_importing_on(
        "texture-import-once-under-test",
        Ok(host_device.physical_device_uuid()),
    );

    let imports =
        check_out_frames_round_robin(&share, &exchange_client, &slot_ids, 5, |checked_out| {
            let HelperCheckedOutSurface::Texture(texture_surface) = checked_out else {
                panic!("a texture registration must check out as a texture");
            };
            Arc::clone(texture_surface.texture_pool_slot_import())
        });
    assert!(!Arc::ptr_eq(&imports[0], &imports[1]));
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        2,
        "five texture frames over two slots import twice"
    );
}

/// Five frames over a two-slot pool of tensor storage buffers import each
/// slot once and keep its memory for CUDA to adopt on first use. Needs a
/// device that exports DEVICE_LOCAL OPAQUE_FD memory; skips where none does.
#[test]
fn five_storage_buffer_frames_over_a_two_slot_pool_import_each_slot_once() {
    use streamlib::sdk::engine::HostSurfaceStoreExt as _;
    use streamlib::sdk::rhi::{StorageBuffer, TensorElementType, TensorStorageBufferLayout};

    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let tensor_layout = TensorStorageBufferLayout::new(vec![4, 16, 16], TensorElementType::Float32)
        .expect("a valid tensor layout");
    let mut storage_buffers = Vec::new();
    for _ in 0..2 {
        match HostVulkanBuffer::new_opaque_fd_export_device_local(
            &host_device,
            tensor_layout.byte_size(),
        ) {
            Ok(exported) => {
                storage_buffers.push(StorageBuffer::from_host_vulkan_buffer(Arc::new(exported)))
            }
            Err(unavailable) => {
                tracing::warn!(
                    "skipping: no DEVICE_LOCAL OPAQUE_FD export on this device ({unavailable})"
                );
                return;
            }
        }
    }
    let share = SurfaceShareUnderTest::start("storage-buffer-import-once");
    let engine_surface_store = SurfaceStore::new(
        share.socket_path.to_string_lossy().into_owned(),
        "engine:storage-buffer-import-once-under-test".to_string(),
    );
    engine_surface_store
        .connect()
        .expect("the engine's store connects");
    let slot_ids = [
        "storage-buffer-import-once-slot-a",
        "storage-buffer-import-once-slot-b",
    ];
    for (slot_id, storage_buffer) in slot_ids.iter().zip(&storage_buffers) {
        engine_surface_store
            .register_storage_buffer(slot_id, storage_buffer, &tensor_layout)
            .expect("the engine registers the storage buffer pool slot");
    }
    let exchange_client = share.exchange_client_labelled_importing_on(
        "storage-buffer-import-once-under-test",
        Ok(host_device.physical_device_uuid()),
    );

    let imports =
        check_out_frames_round_robin(&share, &exchange_client, &slot_ids, 5, |checked_out| {
            let HelperCheckedOutSurface::StorageBuffer(storage_buffer) = checked_out else {
                panic!("a storage_buffer registration must check out as a storage buffer");
            };
            assert_eq!(storage_buffer.tensor_layout, tensor_layout);
            Arc::clone(storage_buffer.storage_buffer_pool_slot_import())
        });
    assert!(!Arc::ptr_eq(&imports[0], &imports[1]));
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        2,
        "five storage buffer frames over two slots import twice"
    );
}
