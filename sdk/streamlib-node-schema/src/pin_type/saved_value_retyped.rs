// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A saved value carried into another pin type through the one list of conversions the link
//! verdict reads: the one conversion that fits the two types casts it, and the built-in number
//! conversions cast with [`the_number_cast`].

use std::fmt;

use serde_json::Value;

use super::pin_link_verdict::takes_as_it_is;
use super::stored_value_check::integer_bounds;
use super::{FloatSize, PinConversion, PinType, PinValueNotTaken};
use crate::NodeClassImportPath;

/// What becomes of a saved value of one type when its pin takes another.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedValueRetyped {
    /// The one conversion that fits cast it into `value`.
    CastBy {
        /// The conversion's node type.
        conversion: NodeClassImportPath,
        /// The value cast.
        value: Value,
    },
    /// No conversion fits, and the new type takes the value as it is.
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
    /// The one conversion that fits converts only inside a started stream.
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
}

impl WhyASavedValueTakesTheDefault {
    /// Each conversion that fits the two types, by its node type.
    pub fn conversions_that_fit(&self) -> Vec<&NodeClassImportPath> {
        match self {
            Self::NoConversionFits => Vec::new(),
            Self::MoreThanOneConversionFits(conversions) => conversions.iter().collect(),
            Self::TheConversionRunsOnlyInAStartedStream(conversion)
            | Self::TheConversionRefusedTheValue { conversion, .. }
            | Self::TheCastIsNotTaken { conversion, .. } => vec![conversion],
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
                 it cannot cast a saved value"
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
        }
    }
}

/// What becomes of `value`, a saved value of `from`, once its pin takes `into`, by
/// `conversions`: the one whose input takes `from` as it is and whose output gives `into` as it
/// is casts it, when it supplies a cast; with none, a value `into` takes as it is is kept; any
/// other gives way to `into`'s default, saying why.
pub fn the_saved_value_retyped(
    value: &Value,
    from: &PinType,
    into: &PinType,
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
    let why = match fitting.as_slice() {
        [] if into.check_value(value).is_ok() => return SavedValueRetyped::TakenAsItIs,
        [] => WhyASavedValueTakesTheDefault::NoConversionFits,
        [conversion] => {
            let node_type = conversion.node_type.clone();
            match conversion.saved_value_cast {
                None => {
                    WhyASavedValueTakesTheDefault::TheConversionRunsOnlyInAStartedStream(node_type)
                }
                Some(saved_value_cast) => match (saved_value_cast.cast)(value, into) {
                    Err(why) => WhyASavedValueTakesTheDefault::TheConversionRefusedTheValue {
                        conversion: node_type,
                        why,
                    },
                    Ok(cast) => match into.check_value(&cast) {
                        Ok(()) => {
                            return SavedValueRetyped::CastBy {
                                conversion: node_type,
                                value: cast,
                            };
                        }
                        Err(not_taken) => WhyASavedValueTakesTheDefault::TheCastIsNotTaken {
                            conversion: node_type,
                            value: cast,
                            not_taken,
                        },
                    },
                },
            }
        }
        several => WhyASavedValueTakesTheDefault::MoreThanOneConversionFits(
            several
                .iter()
                .map(|conversion| conversion.node_type.clone())
                .collect(),
        ),
    };
    SavedValueRetyped::TakesTheDefault(why)
}

/// `value`, a Boolean, an Integer or a Float, cast into a value of `into`, another of them: a
/// Float into an Integer truncated toward zero and held to the Integer's size, an Integer into a
/// Float exactly, a Boolean into a number as 0 or 1, and a number into a Boolean as `true`
/// unless it is 0; or why it is not cast.
pub fn the_number_cast(value: &Value, into: &PinType) -> Result<Value, String> {
    let number = match value {
        Value::Bool(holds) => NumberCast::Whole(i128::from(*holds)),
        Value::Number(number) => match number.as_i64().map(i128::from) {
            Some(whole) => NumberCast::Whole(whole),
            None => match number.as_u64() {
                Some(whole) => NumberCast::Whole(i128::from(whole)),
                None => NumberCast::Fractional(number.as_f64().unwrap_or_default()),
            },
        },
        other => return Err(format!("{other} is no Boolean, Integer or Float")),
    };
    match into {
        PinType::Boolean {} => Ok(Value::Bool(match number {
            NumberCast::Whole(whole) => whole != 0,
            NumberCast::Fractional(fractional) => fractional != 0.0,
        })),
        PinType::Integer { exact_size } => {
            let (lowest, highest) = integer_bounds(*exact_size);
            let whole = match number {
                NumberCast::Whole(whole) => whole,
                NumberCast::Fractional(fractional) => {
                    let truncated = fractional.trunc();
                    if truncated <= lowest as f64 {
                        lowest
                    } else if truncated >= highest as f64 {
                        highest
                    } else {
                        truncated as i128
                    }
                }
            };
            Ok(a_whole_number(whole.clamp(lowest, highest)))
        }
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

/// `value`, a value of `pin_type`, held within `[lowest, highest]` when it is a number: an
/// Integer within the whole numbers the range holds, when it holds any; any other value as it is.
pub fn the_value_held_within_the_range(
    value: Value,
    pin_type: &PinType,
    (lowest, highest): (f64, f64),
) -> Value {
    match pin_type {
        PinType::Integer { .. } => {
            let (lowest, highest) = (lowest.ceil(), highest.floor());
            match value.as_f64() {
                Some(whole) if lowest <= highest && (whole < lowest || whole > highest) => {
                    a_whole_number(whole.clamp(lowest, highest) as i128)
                }
                _ => value,
            }
        }
        PinType::Float { .. } => match value.as_f64() {
            Some(fractional)
                if lowest <= highest && (fractional < lowest || fractional > highest) =>
            {
                serde_json::Number::from_f64(fractional.clamp(lowest, highest))
                    .map_or(value, Value::Number)
            }
            _ => value,
        },
        _ => value,
    }
}

/// A number read for a cast: whole, or with a fraction.
#[derive(Clone, Copy)]
enum NumberCast {
    Whole(i128),
    Fractional(f64),
}

/// `whole` as a JSON number, which holds any whole number an Integer of any size holds.
fn a_whole_number(whole: i128) -> Value {
    match (i64::try_from(whole), u64::try_from(whole)) {
        (Ok(signed), _) => Value::from(signed),
        (_, Ok(unsigned)) => Value::from(unsigned),
        _ => Value::from(if whole < 0 { i64::MIN } else { i64::MAX }),
    }
}
