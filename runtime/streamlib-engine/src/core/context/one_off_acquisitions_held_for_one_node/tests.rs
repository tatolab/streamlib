// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::one_off_acquisition_shapes_for_tests::{
    a_640x480_bgra32_pixel_buffer, a_1280x720_rgba8_unorm_texture,
};
use super::{
    ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE, OneOffAcquisitionShape,
    OneOffAcquisitionsHeldForOneNode,
};
use crate::core::rhi::{
    TensorElementType, TensorStorageBufferLayout, TextureFormat, TextureUsages,
};

#[test]
fn a_one_off_acquisition_of_another_shape_is_counted_on_its_own() {
    let mut held: OneOffAcquisitionsHeldForOneNode<&'static str> =
        OneOffAcquisitionsHeldForOneNode::default();
    let texture_binding = a_1280x720_rgba8_unorm_texture();
    let storage_binding = OneOffAcquisitionShape::Texture {
        width: 1280,
        height: 720,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::STORAGE_BINDING,
    };
    for _ in 0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE {
        held.reserve_one_more_of_shape(&texture_binding)
            .expect("up to the count");
    }
    assert!(held.reserve_one_more_of_shape(&texture_binding).is_err());

    held.reserve_one_more_of_shape(&storage_binding)
        .expect("a usage is part of the shape, so another usage is counted on its own");
    assert_eq!(held.held_or_reserved_count_of_shape(&storage_binding), 1);
    assert_eq!(
        held.held_or_reserved_count_of_shape(&texture_binding),
        ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE
    );
}

#[test]
fn holding_a_reserved_one_off_keeps_its_count_until_it_is_released() {
    let mut held: OneOffAcquisitionsHeldForOneNode<&'static str> =
        OneOffAcquisitionsHeldForOneNode::default();
    let shape = a_640x480_bgra32_pixel_buffer();

    held.reserve_one_more_of_shape(&shape).expect("the first");
    assert!(
        held.hold_a_reserved_acquisition("handle-0".to_string(), shape.clone(), "held-0")
            .is_none()
    );
    assert_eq!(held.held_count(), 1);
    assert_eq!(held.held_or_reserved_count_of_shape(&shape), 1);
    assert_eq!(held.held_acquisition("handle-0"), Some(&"held-0"));

    for _ in 1..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE {
        held.reserve_one_more_of_shape(&shape)
            .expect("up to the count");
    }
    assert!(
        held.reserve_one_more_of_shape(&shape).is_err(),
        "a held one-off counts toward the node's count of its shape"
    );

    assert_eq!(held.release_held_acquisition("handle-0"), Some("held-0"));
    assert_eq!(held.held_count(), 0);
    assert!(held.release_held_acquisition("handle-0").is_none());
    held.reserve_one_more_of_shape(&shape)
        .expect("releasing a held one-off hands its count back");
}

#[test]
fn holding_a_one_off_under_a_handle_id_already_held_hands_back_the_replaced_ones_count() {
    let mut held: OneOffAcquisitionsHeldForOneNode<&'static str> =
        OneOffAcquisitionsHeldForOneNode::default();
    let texture = a_1280x720_rgba8_unorm_texture();
    let pixel_buffer = a_640x480_bgra32_pixel_buffer();

    held.reserve_one_more_of_shape(&texture).unwrap();
    held.hold_a_reserved_acquisition("one-id".to_string(), texture.clone(), "the texture");
    held.reserve_one_more_of_shape(&pixel_buffer).unwrap();
    let replaced =
        held.hold_a_reserved_acquisition("one-id".to_string(), pixel_buffer.clone(), "the buffer");

    assert_eq!(replaced, Some("the texture"));
    assert_eq!(held.held_count(), 1);
    assert_eq!(held.held_or_reserved_count_of_shape(&texture), 0);
    assert_eq!(held.held_or_reserved_count_of_shape(&pixel_buffer), 1);
}

#[test]
fn releasing_every_held_one_off_hands_back_every_count_it_held() {
    let mut held: OneOffAcquisitionsHeldForOneNode<&'static str> =
        OneOffAcquisitionsHeldForOneNode::default();
    let texture = a_1280x720_rgba8_unorm_texture();
    let pixel_buffer = a_640x480_bgra32_pixel_buffer();
    for (handle_id, shape) in [
        ("texture-0", &texture),
        ("texture-1", &texture),
        ("buffer-0", &pixel_buffer),
    ] {
        held.reserve_one_more_of_shape(shape).unwrap();
        held.hold_a_reserved_acquisition(handle_id.to_string(), shape.clone(), handle_id);
    }
    held.reserve_one_more_of_shape(&texture)
        .expect("a reservation still on its way to being held");

    let mut released: Vec<String> = held
        .release_every_held_acquisition()
        .into_iter()
        .map(|(handle_id, _)| handle_id)
        .collect();
    released.sort();

    assert_eq!(released, ["buffer-0", "texture-0", "texture-1"]);
    assert_eq!(held.held_count(), 0);
    assert_eq!(
        held.held_or_reserved_count_of_shape(&texture),
        1,
        "the unheld reservation keeps its count until it is held or handed back"
    );
    assert_eq!(held.held_or_reserved_count_of_shape(&pixel_buffer), 0);
    held.hand_back_an_unheld_reservation_of_shape(&texture);
    assert_eq!(held.held_or_reserved_count_of_shape(&texture), 0);
}

#[test]
fn a_one_off_shape_reads_its_extent_format_and_usage() {
    assert_eq!(
        OneOffAcquisitionShape::Texture {
            width: 1280,
            height: 720,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST,
        }
        .to_string(),
        "1280x720 rgba8_unorm texture (usage copy_src|copy_dst|texture_binding)"
    );
    assert_eq!(
        a_640x480_bgra32_pixel_buffer().to_string(),
        "640x480 bgra32 pixel buffer"
    );
    assert_eq!(
        OneOffAcquisitionShape::StorageBuffer {
            tensor_layout: TensorStorageBufferLayout::new(
                vec![1, 3, 640, 640],
                TensorElementType::Float32
            )
            .expect("a valid tensor"),
        }
        .to_string(),
        "[1, 3, 640, 640] float32 storage buffer"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        OneOffAcquisitionShape::RenderTargetImage {
            width: 1280,
            height: 720,
            format: TextureFormat::Bgra8Unorm,
        }
        .to_string(),
        "1280x720 bgra8_unorm render-target image"
    );
}
