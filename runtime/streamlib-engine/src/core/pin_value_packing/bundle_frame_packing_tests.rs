// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};
use streamlib_ipc_types::{
    BundleFrameDescriptorBody, BundleFrameRowHolds, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody,
    ImageFrameH273Colour, bundle_frame_row_table_of,
};
use streamlib_node_schema::pin_type::BundleMember;

use super::*;

fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

fn a_texture() -> PinType {
    serde_json::from_value(json!({"type": "texture", "pixel_formats": []})).unwrap()
}

/// A picture with the region of it that matters, the bundle a node sends a frame with.
fn a_framed_picture() -> PinType {
    PinType::Bundle {
        name: None,
        members: vec![
            a_member("picture", a_texture()),
            BundleMember {
                default_value: Some(json!({"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0})),
                ..a_member("region", PinType::Rectangle {})
            },
            a_member("label", PinType::String {}),
        ],
    }
}

/// The Image frame descriptor of a picture in slot `slot` of one pool, at its `generation`.
fn an_image_frame(slot: u32, generation: u64) -> DataChannelPayloadDescriptor {
    DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
        slot: GpuPoolSlotFrameDescriptorBody {
            pool: [7; 16],
            slot,
            generation,
            timeline_value: 3,
            image_layout: 5,
        },
        width: 64,
        height: 32,
        format: 2,
        colour: ImageFrameH273Colour::UNSPECIFIED,
    })
}

fn the_picture(descriptor: DataChannelPayloadDescriptor) -> HeavyMemberOfABundleFrame<'static> {
    HeavyMemberOfABundleFrame {
        name: "picture",
        descriptor,
        payload: &[],
    }
}

/// A bundle holding a Texture packs as one Bundle frame: the small members' Records row first,
/// each one left out given its default, then the Texture's row under the very Image frame
/// descriptor it would travel under on its own wire, naming its pool slot and holding no bytes.
/// Read as the same bundle, both come back as they went in.
///
/// Mental-revert: packing the small members by the whole bundle's type refuses the frame, as a
/// Texture member has no stored value; dropping the row's descriptor reads back another slot.
#[test]
fn a_bundle_holding_a_texture_packs_as_one_bundle_frame_and_reads_back_whole() {
    let picture = an_image_frame(4, 19);
    let packed = the_bundle_frame_of_a_bundle(
        &a_framed_picture(),
        &json!({"label": "cam"}),
        &[the_picture(picture)],
    )
    .unwrap();

    assert_eq!(
        packed.payload_descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::BundleFrame)
    );
    let rows = bundle_frame_row_table_of(
        BundleFrameDescriptorBody::from_descriptor_body(&packed.payload_descriptor.body),
        &packed.payload,
    )
    .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.holds.clone()).collect::<Vec<_>>(),
        vec![
            BundleFrameRowHolds::SmallMembers,
            BundleFrameRowHolds::Member("picture".to_owned())
        ]
    );
    assert_eq!(rows[1].descriptor, picture);
    assert_eq!(rows[1].payload_byte_len, 0);

    let read = the_bundle_a_bundle_frame_carries(&a_framed_picture(), packed.as_ref()).unwrap();
    assert_eq!(
        Value::Object(read.small_members),
        json!({
            "region": {"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0},
            "label": "cam",
        })
    );
    assert_eq!(read.heavy_members.len(), 1);
    assert_eq!(read.heavy_members[0].name, "picture");
    assert_eq!(read.heavy_members[0].descriptor, picture);
}

/// An input reads the members it declares: one taking only the picture reads a frame that also
/// carries a region and a label, and one declaring a member the frame has no row for is refused
/// by that member's name.
#[test]
fn a_frame_is_read_as_the_inputs_own_bundle() {
    let packed = the_bundle_frame_of_a_bundle(
        &a_framed_picture(),
        &json!({"label": "cam"}),
        &[the_picture(an_image_frame(1, 1))],
    )
    .unwrap();
    let only_the_picture = PinType::Bundle {
        name: None,
        members: vec![a_member("picture", a_texture())],
    };
    let read = the_bundle_a_bundle_frame_carries(&only_the_picture, packed.as_ref()).unwrap();
    assert!(read.small_members.is_empty());
    assert_eq!(read.heavy_members[0].descriptor, an_image_frame(1, 1));

    let with_a_mask = PinType::Bundle {
        name: None,
        members: vec![
            a_member("picture", a_texture()),
            a_member("mask", a_texture()),
        ],
    };
    let refusal = the_bundle_a_bundle_frame_carries(&with_a_mask, packed.as_ref()).unwrap_err();
    assert!(refusal.contains("no row for member `mask`"), "{refusal}");
}

/// A bundle of a Texture alone packs with no Records row, and reads back with no small member.
#[test]
fn a_bundle_of_a_texture_alone_packs_no_records_row() {
    let only_the_picture = PinType::Bundle {
        name: None,
        members: vec![a_member("picture", a_texture())],
    };
    let packed = the_bundle_frame_of_a_bundle(
        &only_the_picture,
        &json!({}),
        &[the_picture(an_image_frame(2, 8))],
    )
    .unwrap();
    let body = BundleFrameDescriptorBody::from_descriptor_body(&packed.payload_descriptor.body);
    assert_eq!(body.row_count, 1);
    let read = the_bundle_a_bundle_frame_carries(&only_the_picture, packed.as_ref()).unwrap();
    assert!(read.small_members.is_empty());
    assert_eq!(read.heavy_members[0].descriptor, an_image_frame(2, 8));
}

/// Each way a frame's members can be wrong is refused by the member's name: a Texture left out,
/// given twice, given as another kind of message, or one the bundle does not declare, and a
/// small member left out that has no default.
#[test]
fn a_heavy_member_given_wrong_is_refused_by_its_name() {
    let bundle = a_framed_picture();
    let label = json!({"label": "cam"});
    let refusal_of = |small: &Value, heavy: &[HeavyMemberOfABundleFrame<'_>]| {
        the_bundle_frame_of_a_bundle(&bundle, small, heavy).unwrap_err()
    };

    let left_out = refusal_of(&label, &[]);
    assert!(left_out.contains("member `picture` once"), "{left_out}");
    let twice = refusal_of(
        &label,
        &[
            the_picture(an_image_frame(1, 1)),
            the_picture(an_image_frame(2, 1)),
        ],
    );
    assert!(twice.contains("given 2 times"), "{twice}");
    let as_records = refusal_of(
        &label,
        &[the_picture(DataChannelPayloadDescriptor::records_message())],
    );
    assert!(as_records.contains("member `picture`"), "{as_records}");
    assert!(as_records.contains("ImageFrame"), "{as_records}");
    let unknown = refusal_of(
        &label,
        &[
            the_picture(an_image_frame(1, 1)),
            HeavyMemberOfABundleFrame {
                name: "mask",
                ..the_picture(an_image_frame(2, 1))
            },
        ],
    );
    assert!(unknown.contains("member `mask`"), "{unknown}");
    let no_label = refusal_of(&json!({}), &[the_picture(an_image_frame(1, 1))]);
    assert!(no_label.contains("`label`"), "{no_label}");
}

/// A frame is read only off a Bundle frame, and only for a bundle holding a heavy member: a
/// Records message is refused as a frame, and a bundle of small members never travels as one.
#[test]
fn only_a_bundle_holding_a_heavy_member_travels_as_a_bundle_frame() {
    let records = PackedPinValue {
        payload_descriptor: DataChannelPayloadDescriptor::records_message(),
        payload: rmp_serde::to_vec_named(&json!({"label": "cam"})).unwrap(),
    };
    let refusal =
        the_bundle_a_bundle_frame_carries(&a_framed_picture(), records.as_ref()).unwrap_err();
    assert!(refusal.contains("travels as a Bundle frame"), "{refusal}");

    let small_only = PinType::Bundle {
        name: None,
        members: vec![a_member("label", PinType::String {})],
    };
    let refusal =
        the_bundle_frame_of_a_bundle(&small_only, &json!({"label": "cam"}), &[]).unwrap_err();
    assert!(refusal.contains("never as a Bundle frame"), "{refusal}");
}
