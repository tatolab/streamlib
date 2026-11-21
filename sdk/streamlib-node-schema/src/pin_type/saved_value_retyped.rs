// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A saved value carried into another pin type through the one list of conversions the link
//! verdict reads: the one conversion that fits the two types casts it, and the built-in number
//! conversions cast with [`the_number_cast`].

use std::fmt;

use serde_json::Value;

use super::pin_link_verdict::takes_as_it_is;
use super::stored_value_check::{a_whole_json_number, integer_bounds, the_whole_number_of};
use super::{FloatSize, IntegerSize, PinConversion, PinNumberRange, PinType, PinValueNotTaken};
use crate::NodeClassImportPath;

/// What becomes of a saved value of one type when its pin takes another.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedValueRetyped {
    /// The one conversion that fits cast it into `value`.
    CastBy {
        /// The conversion's node type.
        conversion: NodeClassImportPath,
        /// The value cast, held within the pin's range.
        value: Value,
    },
    /// The new type takes the value as it is, and no conversion casts it.
    TakenAsItIs,
    /// The value gives way to the new type's default, which is no saved value.
    TakesTheDefault(WhyASavedValueTakesTheDefault),
}

/// Why a saved value gives way to its new type's default.
#[derive(Debug, Clone, PartialEq)]
pub enum WhyASavedValueTakesTheDefault {
    /// No conversion fits the two types, and the new type does not take the value as it is.
    NoConversionFits,
    /// More than one conversion fits, and none is picked over the others.
    MoreThanOneConversionFits(Vec<NodeClassImportPath>),
    /// The one conversion that fits converts only inside a started stream, and the new type
    /// does not take the value as it is.
    TheConversionRunsOnlyInAStartedStream(NodeClassImportPath),
    /// The one conversion that fits refused the value, in its own words.
    TheConversionRefusedTheValue {
        /// The conversion's node type.
        conversion: NodeClassImportPath,
        /// Why.
        why: String,
    },
    /// The one conversion that fits gave a value the new type does not take.
    TheCastIsNotTaken {
        /// The conversion's node type.
        conversion: NodeClassImportPath,
        /// The value it gave.
        value: Value,
        /// What the new type takes.
        not_taken: PinValueNotTaken,
    },
    /// The one conversion that fits gave an Integer, and the pin's range holds no whole number.
    NoWholeNumberIsWithinTheRange {
        /// The conversion's node type.
        conversion: NodeClassImportPath,
        /// The pin's range.
        range: PinNumberRange,
    },
}

impl WhyASavedValueTakesTheDefault {
    /// Each conversion that fits the two types, by its node type.
    pub fn conversions_that_fit(&self) -> &[NodeClassImportPath] {
        match self {
            Self::NoConversionFits => &[],
            Self::MoreThanOneConversionFits(conversions) => conversions,
            Self::TheConversionRunsOnlyInAStartedStream(conversion)
            | Self::TheConversionRefusedTheValue { conversion, .. }
            | Self::TheCastIsNotTaken { conversion, .. }
            | Self::NoWholeNumberIsWithinTheRange { conversion, .. } => {
                std::slice::from_ref(conversion)
            }
        }
    }
}

impl fmt::Display for WhyASavedValueTakesTheDefault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConversionFits => formatter.write_str(
                "no conversion the stream registers converts between the two types, and the new \
                 type does not take the value as it is",
            ),
            Self::MoreThanOneConversionFits(conversions) => write!(
                formatter,
                "{} each convert between the two types, and none is picked over the others",
                conversions
                    .iter()
                    .map(|conversion| format!("`{conversion}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::TheConversionRunsOnlyInAStartedStream(conversion) => write!(
                formatter,
                "`{conversion}` converts between the two types only inside a started stream, so \
                 it cannot cast a saved value, and the new type does not take the value as it is"
            ),
            Self::TheConversionRefusedTheValue { conversion, why } => {
                write!(formatter, "`{conversion}` did not cast it: {why}")
            }
            Self::TheCastIsNotTaken {
                conversion,
                value,
                not_taken,
            } => write!(
                formatter,
                "`{conversion}` cast it to {value}, and the new type takes {}",
                not_taken.takes
            ),
            Self::NoWholeNumberIsWithinTheRange { conversion, range } => write!(
                formatter,
                "`{conversion}` cast it to an Integer, and the range from {} to {} holds no \
                 whole number",
                in_words(range.minimum),
                in_words(range.maximum)
            ),
        }
    }
}

/// A bound of a range in words: the number, or `none` when it states none.
fn in_words(bound: Option<f64>) -> String {
    bound.map_or_else(|| "none".to_owned(), |bound| bound.to_string())
}

/// What becomes of `value`, a saved value of `from`, once its pin takes `into` within `range`,
/// by `conversions`: the one whose input takes `from` as it is and whose output gives `into` as
/// it is casts it, when it supplies a cast, the cast held within `range`; a value `into` takes as
/// it is is kept when no conversion casts it; any other gives way to `into`'s default, saying
/// why.
pub fn the_saved_value_retyped(
    value: &Value,
    from: &PinType,
    into: &PinType,
    range: Option<&PinNumberRange>,
    conversions: &[PinConversion],
) -> SavedValueRetyped {
    let fitting: Vec<&PinConversion> = conversions
        .iter()
        .filter(|conversion| {
            takes_as_it_is(from, &conversion.input_pin_type)
                && conversion
                    .output_picked_for(into)
                    .is_some_and(|picked| takes_as_it_is(&picked.output_pin_type, into))
        })
        .collect();
    let taken_as_it_is = into.check_value(value).is_ok();
    match fitting.as_slice() {
        [] if taken_as_it_is => SavedValueRetyped::TakenAsItIs,
        [] => SavedValueRetyped::TakesTheDefault(WhyASavedValueTakesTheDefault::NoConversionFits),
        [only] => cast_by_the_one_that_fits(value, into, range, only, taken_as_it_is),
        several => SavedValueRetyped::TakesTheDefault(
            WhyASavedValueTakesTheDefault::MoreThanOneConversionFits(
                several
                    .iter()
                    .map(|conversion| conversion.node_type.clone())
                    .collect(),
            ),
        ),
    }
}

/// What becomes of `value` cast into `into` within `range` by `only`, the one conversion that
/// fits: with no cast of its own, it is kept when `taken_as_it_is`, as when none fits.
fn cast_by_the_one_that_fits(
    value: &Value,
    into: &PinType,
    range: Option<&PinNumberRange>,
    only: &PinConversion,
    taken_as_it_is: bool,
) -> SavedValueRetyped {
    use WhyASavedValueTakesTheDefault as Why;
    let conversion = only.node_type.clone();
    let Some(saved_value_cast) = only.saved_value_cast else {
        if taken_as_it_is {
            return SavedValueRetyped::TakenAsItIs;
        }
        return SavedValueRetyped::TakesTheDefault(Why::TheConversionRunsOnlyInAStartedStream(
            conversion,
        ));
    };
    let cast = match (saved_value_cast.cast)(value, into) {
        Ok(cast) => cast,
        Err(why) => {
            return SavedValueRetyped::TakesTheDefault(Why::TheConversionRefusedTheValue {
                conversion,
                why,
            });
        }
    };
    if let Err(not_taken) = into.check_value(&cast) {
        return SavedValueRetyped::TakesTheDefault(Why::TheCastIsNotTaken {
            conversion,
            value: cast,
            not_taken,
        });
    }
    let value = match range {
        None => cast,
        Some(range) => {
            let Some(held) = the_value_held_within_the_range(cast, into, range) else {
                return SavedValueRetyped::TakesTheDefault(Why::NoWholeNumberIsWithinTheRange {
                    conversion,
                    range: *range,
                });
            };
            held
        }
    };
    SavedValueRetyped::CastBy { conversion, value }
}

/// `fractional`, a Float, cast into an Integer of `exact_size`: truncated toward zero, then held
/// to the whole numbers that size holds.
pub fn the_integer_a_float_is_cast_to(fractional: f64, exact_size: Option<IntegerSize>) -> Value {
    let (lowest, highest) = integer_bounds(exact_size);
    a_whole_json_number((fractional.trunc() as i128).clamp(lowest, highest))
}

/// `value`, a Boolean, an Integer or a Float, cast into a value of `into`, another of them: a
/// Float into an Integer as [`the_integer_a_float_is_cast_to`] casts it, an Integer into a Float
/// exactly, a Boolean into a number as 0 or 1, and a number into a Boolean as `true` unless it is
/// 0; or why it is not cast.
pub fn the_number_cast(value: &Value, into: &PinType) -> Result<Value, String> {
    let number = match value {
        Value::Bool(holds) => NumberCast::Whole(i128::from(*holds)),
        Value::Number(number) => match the_whole_number_of(number) {
            Some(whole) => NumberCast::Whole(whole),
            None => NumberCast::Fractional(
                number
                    .as_f64()
                    .ok_or_else(|| format!("{number} is no number a Float holds"))?,
            ),
        },
        other => return Err(format!("{other} is no Boolean, Integer or Float")),
    };
    match into {
        PinType::Boolean {} => Ok(Value::Bool(match number {
            NumberCast::Whole(whole) => whole != 0,
            NumberCast::Fractional(fractional) => fractional != 0.0,
        })),
        PinType::Integer { exact_size } => Ok(match number {
            NumberCast::Whole(whole) => {
                let (lowest, highest) = integer_bounds(*exact_size);
                a_whole_json_number(whole.clamp(lowest, highest))
            }
            NumberCast::Fractional(fractional) => {
                the_integer_a_float_is_cast_to(fractional, *exact_size)
            }
        }),
        PinType::Float { exact_size } => {
            let fractional = match number {
                NumberCast::Whole(whole) => whole as f64,
                NumberCast::Fractional(fractional) => fractional,
            };
            let fractional = match exact_size {
                Some(FloatSize::Float32) => {
                    fractional.clamp(f64::from(f32::MIN), f64::from(f32::MAX))
                }
                _ => fractional,
            };
            serde_json::Number::from_f64(fractional)
                .map(Value::Number)
                .ok_or_else(|| format!("{fractional} is no finite Float"))
        }
        other => Err(format!("a {other} is no Boolean, Integer or Float")),
    }
}

/// `value`, a value of `pin_type`, held within `range` when it is a number: an Integer within
/// the whole numbers the range holds, `None` when it holds none; any other value as it is.
pub fn the_value_held_within_the_range(
    value: Value,
    pin_type: &PinType,
    range: &PinNumberRange,
) -> Option<Value> {
    match pin_type {
        PinType::Integer { .. } => {
            let Some(whole) = value.as_number().and_then(the_whole_number_of) else {
                return Some(value);
            };
            let lowest = range.minimum.map(|lowest| lowest.ceil() as i128);
            let highest = range.maximum.map(|highest| highest.floor() as i128);
            if let (Some(lowest), Some(highest)) = (lowest, highest)
                && lowest > highest
            {
                return None;
            }
            let whole = lowest.map_or(whole, |lowest| whole.max(lowest));
            let whole = highest.map_or(whole, |highest| whole.min(highest));
            Some(a_whole_json_number(whole))
        }
        PinType::Float { .. } => {
            let Some(fractional) = value.as_f64() else {
                return Some(value);
            };
            let held = range
                .minimum
                .map_or(fractional, |lowest| fractional.max(lowest));
            let held = range.maximum.map_or(held, |highest| held.min(highest));
            if held == fractional {
                return Some(value);
            }
            Some(serde_json::Number::from_f64(held).map_or(value, Value::Number))
        }
        _ => Some(value),
    }
}

/// A number read for a cast: whole, or with a fraction.
#[derive(Clone, Copy)]
enum NumberCast {
    Whole(i128),
    Fractional(f64),
}
