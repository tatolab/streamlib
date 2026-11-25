// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Which slot count a pin group has, read off the inputs and outputs of its slot.

use crate::descriptors::PortDescriptor;
use crate::pin_group::{PinGroupDescriptor, PinGroupSlotCount};
use crate::pin_type::{NodeInputMode, PinDeclaration, PinType, SettingReadTime};

fn a_float_input(input_mode: Option<NodeInputMode>) -> PortDescriptor {
    let input = PortDescriptor::new(
        "opacity",
        "",
        false,
        PinDeclaration::exactly(PinType::Float { exact_size: None }),
    );
    match input_mode {
        Some(input_mode) => input.with_input_mode(input_mode),
        None => input,
    }
}

fn a_group_of(inputs: Vec<PortDescriptor>, outputs: Vec<PortDescriptor>) -> PinGroupDescriptor {
    PinGroupDescriptor::new("layers", "", inputs, outputs)
}

#[test]
fn a_slot_holding_an_input_that_takes_a_wire_follows_its_wires() {
    for input_mode in [
        None,
        Some(NodeInputMode::WireOrValue),
        Some(NodeInputMode::WireOnly),
    ] {
        assert_eq!(
            a_group_of(vec![a_float_input(input_mode)], Vec::new()).slot_count(),
            PinGroupSlotCount::FollowsItsWires
        );
    }
}

#[test]
fn a_slot_holding_only_settings_or_outputs_has_its_count_set() {
    for read in [SettingReadTime::AtSetup, SettingReadTime::EachRun] {
        let setting = a_float_input(Some(NodeInputMode::SettingOnly { read }));
        assert_eq!(
            a_group_of(vec![setting], Vec::new()).slot_count(),
            PinGroupSlotCount::SetByItsInput
        );
    }
    assert_eq!(
        a_group_of(Vec::new(), vec![a_float_input(None)]).slot_count(),
        PinGroupSlotCount::SetByItsInput
    );
}
