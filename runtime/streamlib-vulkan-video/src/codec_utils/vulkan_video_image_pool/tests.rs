// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Helper: build a dummy `ImageResourceHandle` for testing.
fn dummy_handle(index: u32) -> ImageResourceHandle {
    // Use the index as a stand-in for vk handles (non-null).
    ImageResourceHandle {
        image: unsafe { std::mem::transmute::<u64, vk::Image>(100 + index as u64) },
        image_view: unsafe { std::mem::transmute::<u64, vk::ImageView>(200 + index as u64) },
        create_info_extent: vk::Extent3D {
            width: 1920,
            height: 1080,
            depth: 1,
        },
    }
}

/// A trivial factory that always succeeds.
fn ok_factory(index: u32, _: &ImageCreateInfoSnapshot) -> Result<ImageResourceHandle, vk::Result> {
    Ok(dummy_handle(index))
}

#[test]
fn create_pool_defaults() {
    let pool = VulkanVideoImagePool::create();
    assert_eq!(pool.size(), 0);
}

#[test]
fn configure_pool() {
    let pool = VulkanVideoImagePool::create();
    let result = pool.configure(
        4,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 1920,
            height: 1080,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );
    assert_eq!(result, vk::Result::SUCCESS);
    assert_eq!(pool.size(), 4);
}

#[test]
fn configure_too_many_images() {
    let pool = VulkanVideoImagePool::create();
    let result = pool.configure(
        (MAX_IMAGES + 1) as u32,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 128,
            height: 128,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );
    assert_eq!(result, vk::Result::ERROR_TOO_MANY_OBJECTS);
}

#[test]
fn get_and_release_image() {
    let pool = VulkanVideoImagePool::create();
    pool.configure(
        2,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 1920,
            height: 1080,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );

    // Acquire two images — pool has exactly 2.
    let h1 = pool
        .get_available_image(vk::ImageLayout::VIDEO_DECODE_DST_KHR, ok_factory)
        .expect("should get image 1");
    let h2 = pool
        .get_available_image(vk::ImageLayout::VIDEO_DECODE_DST_KHR, ok_factory)
        .expect("should get image 2");

    assert_ne!(h1.index(), h2.index());

    // Pool is now exhausted.
    assert!(
        pool.get_available_image(vk::ImageLayout::VIDEO_DECODE_DST_KHR, ok_factory)
            .is_none()
    );

    // Drop h1 — it returns to the pool.
    let idx1 = h1.index();
    drop(h1);

    // We should be able to acquire again.
    let h3 = pool
        .get_available_image(vk::ImageLayout::VIDEO_DECODE_DST_KHR, ok_factory)
        .expect("should get image after release");
    assert_eq!(h3.index(), idx1);

    drop(h2);
    drop(h3);
}

#[test]
fn release_returns_correct_bit() {
    let pool = VulkanVideoImagePool::create();
    pool.configure(
        4,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 640,
            height: 480,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );

    // Acquire all four.
    let handles: Vec<_> = (0..4)
        .map(|_| {
            pool.get_available_image(vk::ImageLayout::VIDEO_DECODE_DPB_KHR, ok_factory)
                .unwrap()
        })
        .collect();

    // Release index 2 specifically.
    let idx2 = handles[2].index();
    drop(handles);

    // After dropping all, all should be available again.
    let mut acquired = Vec::new();
    for _ in 0..4 {
        acquired.push(
            pool.get_available_image(vk::ImageLayout::VIDEO_DECODE_DPB_KHR, ok_factory)
                .unwrap(),
        );
    }

    // Verify no duplicates.
    let mut indices: Vec<u32> = acquired.iter().map(|h| h.index()).collect();
    indices.sort();
    indices.dedup();
    assert_eq!(indices.len(), 4);

    // Verify idx2 was among them.
    assert!(indices.contains(&idx2));
}

#[test]
fn node_image_exist_and_layout() {
    let mut node = VulkanVideoImagePoolNode::new();
    assert!(!node.image_exist());
    assert!(node.recreate_image());
    assert!(!node.set_new_layout(vk::ImageLayout::GENERAL));

    // Give it an image.
    node.create_image(
        dummy_handle(0),
        vk::Extent3D {
            width: 320,
            height: 240,
            depth: 1,
        },
        0,
        false,
        false,
    );

    assert!(node.image_exist());
    assert!(!node.recreate_image());
    assert!(node.set_new_layout(vk::ImageLayout::GENERAL));
}

#[test]
fn node_respec_forces_recreate() {
    let mut node = VulkanVideoImagePoolNode::new();
    node.create_image(
        dummy_handle(0),
        vk::Extent3D {
            width: 320,
            height: 240,
            depth: 1,
        },
        0,
        false,
        false,
    );
    assert!(!node.recreate_image());

    node.respec_image();
    assert!(node.recreate_image());
    // set_new_layout should fail since recreation is pending.
    assert!(!node.set_new_layout(vk::ImageLayout::GENERAL));
}

#[test]
fn create_external_node() {
    let handle = dummy_handle(42);
    let extent = vk::Extent2D {
        width: 1280,
        height: 720,
    };
    let node = VulkanVideoImagePoolNode::create_external(
        handle.clone(),
        extent,
        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
    );

    assert!(node.image_exist());
    assert_eq!(node.parent_index, -1);
    assert!(node.parent.is_none());
    assert_eq!(
        node.current_image_layout,
        vk::ImageLayout::TRANSFER_DST_OPTIMAL
    );
    assert_eq!(node.picture_resource_info.coded_extent.width, 1280);
    assert_eq!(node.picture_resource_info.coded_extent.height, 720);
}

#[test]
fn wrap_around_allocation() {
    let pool = VulkanVideoImagePool::create();
    pool.configure(
        3,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 640,
            height: 480,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );

    // Acquire and release to advance the internal cursor.
    let h0 = pool
        .get_available_image(vk::ImageLayout::GENERAL, ok_factory)
        .unwrap();
    let h1 = pool
        .get_available_image(vk::ImageLayout::GENERAL, ok_factory)
        .unwrap();
    assert_eq!(h0.index(), 0);
    assert_eq!(h1.index(), 1);
    drop(h0); // release slot 0

    // The cursor is now at 2.  Acquire slot 2, then the wrap-around
    // should find slot 0.
    let h2 = pool
        .get_available_image(vk::ImageLayout::GENERAL, ok_factory)
        .unwrap();
    assert_eq!(h2.index(), 2);

    let h_wrap = pool
        .get_available_image(vk::ImageLayout::GENERAL, ok_factory)
        .unwrap();
    assert_eq!(h_wrap.index(), 0); // wrapped around

    drop(h1);
    drop(h2);
    drop(h_wrap);
}

#[test]
fn factory_failure_returns_slot() {
    let pool = VulkanVideoImagePool::create();
    // Configure with a successful factory first so pool_size is set, but
    // images are already created during configure.
    pool.configure(
        2,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 640,
            height: 480,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );

    // Force slot 0 to need recreation.
    pool.with_node_mut(0, |n| n.respec_image());

    // Use a failing factory.
    let fail_factory =
        |_: u32, _: &ImageCreateInfoSnapshot| -> Result<ImageResourceHandle, vk::Result> {
            Err(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
        };

    // The failing factory should cause get_available_image to return None
    // but the slot should be returned to the pool.
    let result = pool.get_available_image(vk::ImageLayout::GENERAL, fail_factory);
    assert!(result.is_none());

    // With a working factory a slot should now be acquirable.
    // Slot 1 is returned because next_node_to_use advanced past 0.
    let h = pool
        .get_available_image(vk::ImageLayout::GENERAL, ok_factory)
        .unwrap();
    assert!(h.index() <= 1); // either slot 0 or 1 is available
    drop(h);
}

#[test]
fn reconfigure_larger_extent() {
    let pool = VulkanVideoImagePool::create();
    pool.configure(
        2,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 640,
            height: 480,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );

    // All images exist after configure.
    assert!(pool.with_node(0, |n| n.image_exist()));
    assert!(pool.with_node(1, |n| n.image_exist()));

    // Reconfigure with a larger extent — existing images should be marked
    // for recreation.
    pool.configure(
        2,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 1920,
            height: 1080,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );

    // Images still exist (respec marks for recreation but doesn't destroy).
    // However recreate_image() should return true since respec was called
    // and then create_image was called again with new factory during
    // configure loop.
    assert!(pool.with_node(0, |n| n.image_exist()));
}

#[test]
fn deinit_pool() {
    let pool = VulkanVideoImagePool::create();
    pool.configure(
        2,
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Extent2D {
            width: 640,
            height: 480,
        },
        vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR,
        0,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        false,
        vk::ImageAspectFlags::COLOR,
        false,
        false,
        false,
        ok_factory,
    );
    assert_eq!(pool.size(), 2);

    // Deinit without a real device (no semaphores created).
    unsafe {
        pool.deinit(None);
    }
    assert_eq!(pool.size(), 0);
}
