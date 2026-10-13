// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::cell::Cell;

use super::{
    OneOffAcquisitionsCountedForOneNativeNode, acquire_a_one_off_counted_against_its_node,
};
use crate::core::context::one_off_acquisitions_held_for_one_node::ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE;
use crate::core::context::one_off_acquisitions_held_for_one_node::one_off_acquisition_shapes_for_tests::{
    a_640x480_bgra32_pixel_buffer, a_1280x720_rgba8_unorm_texture,
};
use crate::core::{Error, Result};

fn an_allocation_named(name: &'static str) -> impl FnOnce() -> Result<&'static str> {
    move || Ok(name)
}

#[test]
fn a_native_nodes_seventeenth_held_one_off_of_one_shape_is_refused_by_name_before_it_allocates() {
    let node = OneOffAcquisitionsCountedForOneNativeNode::new();
    let held: Vec<_> = (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
        .map(|_| {
            acquire_a_one_off_counted_against_its_node(
                Some(&node),
                a_1280x720_rgba8_unorm_texture(),
                an_allocation_named("texture"),
            )
            .expect("a native node acquires up to its own count of one shape")
        })
        .collect();
    assert_eq!(held.len(), 16);

    let allocated = Cell::new(false);
    let refusal = acquire_a_one_off_counted_against_its_node(
        Some(&node),
        a_1280x720_rgba8_unorm_texture(),
        || {
            allocated.set(true);
            Ok("the seventeenth texture")
        },
    )
    .expect_err("the seventeenth held one-off of one shape is refused");

    assert!(
        !allocated.get(),
        "the refusal comes before anything allocates"
    );
    assert!(
        matches!(
            refusal,
            Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld {
                one_off_acquisitions_one_node_may_hold_of_one_shape: 16,
                ..
            }
        ),
        "the refusal is the node's own one-off count: {refusal:?}"
    );
    let refusal_text = refusal.to_string();
    assert!(
        refusal_text.contains("1280x720 rgba8_unorm") && refusal_text.contains("16"),
        "the refusal names the shape and the count: {refusal_text}"
    );
    assert_eq!(
        node.held_count_of_shape(&a_1280x720_rgba8_unorm_texture()),
        16,
        "a refused acquisition holds no count"
    );
}

#[test]
fn a_native_nodes_one_off_of_another_shape_is_counted_on_its_own() {
    let node = OneOffAcquisitionsCountedForOneNativeNode::new();
    let _held: Vec<_> = (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
        .map(|_| {
            acquire_a_one_off_counted_against_its_node(
                Some(&node),
                a_1280x720_rgba8_unorm_texture(),
                an_allocation_named("texture"),
            )
            .expect("up to the count")
        })
        .collect();

    let pixel_buffer = acquire_a_one_off_counted_against_its_node(
        Some(&node),
        a_640x480_bgra32_pixel_buffer(),
        an_allocation_named("pixel buffer"),
    )
    .expect("another shape on the same node is counted on its own");

    assert_eq!(*pixel_buffer, "pixel buffer");
    assert_eq!(
        node.held_count_of_shape(&a_640x480_bgra32_pixel_buffer()),
        1
    );
}

#[test]
fn dropping_a_native_nodes_one_off_hands_its_count_back() {
    let node = OneOffAcquisitionsCountedForOneNativeNode::new();
    let mut held: Vec<_> = (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
        .map(|_| {
            acquire_a_one_off_counted_against_its_node(
                Some(&node),
                a_1280x720_rgba8_unorm_texture(),
                an_allocation_named("texture"),
            )
            .expect("up to the count")
        })
        .collect();

    drop(held.pop());
    assert_eq!(
        node.held_count_of_shape(&a_1280x720_rgba8_unorm_texture()),
        15
    );
    let _in_the_room_handed_back = acquire_a_one_off_counted_against_its_node(
        Some(&node),
        a_1280x720_rgba8_unorm_texture(),
        an_allocation_named("texture"),
    )
    .expect("a dropped one-off makes room for one more");

    drop(held);
    assert_eq!(
        node.held_count_of_shape(&a_1280x720_rgba8_unorm_texture()),
        1
    );
}

#[test]
fn a_native_nodes_failed_allocation_hands_its_count_back() {
    let node = OneOffAcquisitionsCountedForOneNativeNode::new();
    acquire_a_one_off_counted_against_its_node(
        Some(&node),
        a_640x480_bgra32_pixel_buffer(),
        || -> Result<()> { Err(Error::Runtime("the device refused".to_string())) },
    )
    .expect_err("the allocation's own refusal comes back");

    assert_eq!(
        node.held_count_of_shape(&a_640x480_bgra32_pixel_buffer()),
        0
    );
}

#[test]
fn one_native_node_holding_sixteen_one_offs_does_not_refuse_another_nodes_acquisition_of_the_same_shape()
 {
    let node_a = OneOffAcquisitionsCountedForOneNativeNode::new();
    let node_b = OneOffAcquisitionsCountedForOneNativeNode::new();
    let _node_a_holds: Vec<_> = (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
        .map(|_| {
            acquire_a_one_off_counted_against_its_node(
                Some(&node_a),
                a_1280x720_rgba8_unorm_texture(),
                an_allocation_named("node a's texture"),
            )
            .expect("up to node a's count")
        })
        .collect();

    let _node_b_holds = acquire_a_one_off_counted_against_its_node(
        Some(&node_b),
        a_1280x720_rgba8_unorm_texture(),
        an_allocation_named("node b's texture"),
    )
    .expect("another node's one-offs of the same shape are never refused by this node's");
    assert_eq!(
        node_b.held_count_of_shape(&a_1280x720_rgba8_unorm_texture()),
        1
    );
}

#[test]
fn the_shared_context_counts_no_one_offs() {
    let held: Vec<_> = (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE + 1)
        .map(|_| {
            acquire_a_one_off_counted_against_its_node(
                None,
                a_1280x720_rgba8_unorm_texture(),
                an_allocation_named("texture"),
            )
            .expect("a context with no node counts nothing, so refuses nothing")
        })
        .collect();

    assert_eq!(held.len(), 17);
}
