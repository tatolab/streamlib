// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Where each value of a compiled call sits among the f64 slots its evaluate entry reads and
//! writes: a number or a token in one slot, a Color, a Vector or a Rectangle in one slot per
//! part, in the order its type packs them.

use streamlib::sdk::error::{Error, Result};

use super::FunctionNodeValue;

/// A value a compiled call carries as its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueCarriedInParts {
    Color,
    Vector { component_count: usize },
    Rectangle,
    Transform,
}

impl ValueCarriedInParts {
    /// The value carried in parts that a function node signature's `value_type` names.
    ///
    /// Wire contract with the helper bootstrap: these value type names.
    pub(super) fn of_the_value_type(value_type: &str) -> Option<Self> {
        Some(match value_type {
            "color" => Self::Color,
            "vector2" => Self::Vector { component_count: 2 },
            "vector3" => Self::Vector { component_count: 3 },
            "vector4" => Self::Vector { component_count: 4 },
            "rectangle" => Self::Rectangle,
            "transform" => Self::Transform,
            _ => return None,
        })
    }

    pub(super) fn part_count(self) -> usize {
        match self {
            Self::Color | Self::Rectangle => 4,
            Self::Vector { component_count } => component_count,
            Self::Transform => 16,
        }
    }

    /// Write into `slots` the parts an input of this value reads of `value`, with the free
    /// conversion a link allows: one number spreads over every component of a Vector, and over
    /// a Color's red, green and blue, opaque; a Vector or a Color of another size keeps the
    /// components it shares and reads each missing one as 0, a Color's alpha as 1. A Rectangle
    /// and a Transform read their parts as they are. Mirrors `parts_a_value_type_reads` of the
    /// helper bootstrap.
    pub(super) fn read_into(
        self,
        slots: &mut [f64],
        value: &FunctionNodeValue,
        node_id: &str,
        input_index: usize,
    ) -> Result<()> {
        let part_count = self.part_count();
        let missing = |index: usize| {
            if self == Self::Color && index == 3 {
                1.0
            } else {
                0.0
            }
        };
        let converts_freely = matches!(self, Self::Color | Self::Vector { .. });
        match value {
            FunctionNodeValue::Number(number) if converts_freely => {
                for (index, slot) in slots.iter_mut().enumerate() {
                    *slot = if self == Self::Color && index == 3 {
                        1.0
                    } else {
                        *number
                    };
                }
                Ok(())
            }
            FunctionNodeValue::Parts(parts) if converts_freely || parts.len() == part_count => {
                for (index, slot) in slots.iter_mut().enumerate() {
                    *slot = parts.get(index).copied().unwrap_or_else(|| missing(index));
                }
                Ok(())
            }
            FunctionNodeValue::Parts(parts) => Err(Error::Runtime(format!(
                "function node `{node_id}` runs compiled and takes {part_count} numbers on \
                 input {input_index}, and was handed a value of {} parts",
                parts.len()
            ))),
            other => Err(Error::Runtime(format!(
                "function node `{node_id}` runs compiled and takes a value of {part_count} parts \
                 on input {input_index}, and was handed {}",
                other.what_it_is()
            ))),
        }
    }
}

/// The f64 slots of each input or output of a compiled call, in port order: where each one's
/// first slot is, and the value it carries in parts, when it does.
#[derive(Debug, Default)]
pub(super) struct SlotsOfEachValue {
    first_slot: Vec<usize>,
    carried_in_parts: Vec<Option<ValueCarriedInParts>>,
    slot_count: usize,
}

impl SlotsOfEachValue {
    /// The slots of `count` values, of which `carried_in_parts` lists each one carried in
    /// parts as `(index, value_type)`.
    pub(super) fn of(
        node_id: &str,
        what_they_are: &str,
        count: usize,
        carried_in_parts: &[(usize, String)],
    ) -> std::result::Result<Self, String> {
        let mut parts_of_each = vec![None; count];
        for (index, value_type) in carried_in_parts {
            let parts = ValueCarriedInParts::of_the_value_type(value_type).ok_or_else(|| {
                format!(
                    "function node `{node_id}` names {what_they_are} {index} as carrying a value \
                     of {value_type}, which carries no parts"
                )
            })?;
            let slot = parts_of_each.get_mut(*index).ok_or_else(|| {
                format!(
                    "function node `{node_id}` names {what_they_are} {index} as carrying parts, \
                     and it has {count}"
                )
            })?;
            *slot = Some(parts);
        }
        let mut first_slot = Vec::with_capacity(count);
        let mut slot_count = 0;
        for parts in &parts_of_each {
            first_slot.push(slot_count);
            slot_count += parts.map_or(1, ValueCarriedInParts::part_count);
        }
        Ok(Self {
            first_slot,
            carried_in_parts: parts_of_each,
            slot_count,
        })
    }

    /// The number of values.
    pub(super) fn count(&self) -> usize {
        self.first_slot.len()
    }

    /// The number of slots every value takes together.
    pub(super) fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// The first slot of value `index`.
    pub(super) fn first_slot_of(&self, index: usize) -> usize {
        self.first_slot[index]
    }

    /// The value `index` carries in parts, when it does.
    pub(super) fn carried_in_parts(&self, index: usize) -> Option<ValueCarriedInParts> {
        self.carried_in_parts.get(index).copied().flatten()
    }
}
