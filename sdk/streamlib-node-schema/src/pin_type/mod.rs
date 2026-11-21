// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a port carries: the closed set of pin types, what a port declares, the JSON form of each
//! type's stored value, how a link between two typed pins connects, and one row per built-in
//! type for the control contract. Behind the `contract-json-schema` feature each type derives
//! the schemars the contract renders.

mod built_in_pin_types;
mod pin_conversion;
mod pin_declaration;
mod pin_flows;
mod pin_link_verdict;
mod pin_type_in_words;
mod pin_type_validation;
mod pin_type_variants;
mod saved_value_retyped;
mod stored_pin_values;
mod stored_value_check;
mod value_json_schema;

pub use built_in_pin_types::{
    BuiltInPinTypeName, PinAnnotationName, PinTypeDescription, built_in_pin_type,
    built_in_pin_types,
};
pub use pin_conversion::{
    PinConversion, PinConversionOutputPick, PinConversionOutputPicked, PinConversionRefusal,
    PinConversionSavedValueCast,
};
pub use pin_declaration::{
    DeclaredPinType, NodeInputMode, PinAnnotations, PinDeclaration, PinFlow, PinNumberRange,
    PinProjectAssetAnnotation, ResolvedPin, SettingReadTime, an_input_takes_a_wire,
};
pub use pin_link_verdict::{
    ConvertNodeToInsert, FreePinConversion, PinLinkEnd, PinLinkEndNamed, PinLinkInputEnd,
    PinLinkOutcome, PinLinkRefusal, PinLinkVerdict, PinLinkWarning, pin_link_verdict,
};
pub use pin_type_variants::{
    BundleMember, FloatSize, IntegerSize, NamedPinTypeImportPath, PinType, ProjectAssetMedia,
    TexturePixelFormat, VectorComponentCount,
};
pub use saved_value_retyped::{
    SavedValueRetyped, WhyASavedValueTakesTheDefault, the_integer_a_float_is_cast_to,
    the_number_cast, the_saved_value_retyped, the_value_held_within_the_range,
};
pub use stored_pin_values::{
    AssetPinValue, BooleanPinValue, BundlePinValue, ColorPinValue, FloatPinValue, IntegerPinValue,
    ListPinValue, MenuPinValue, RectanglePinValue, StringPinValue, TransformPinValue,
    VectorPinValue,
};
pub use stored_value_check::PinValueNotTaken;

#[cfg(test)]
mod pin_conversion_tests;
#[cfg(test)]
mod pin_flows_tests;
#[cfg(test)]
mod pin_link_verdict_tests;
#[cfg(test)]
mod pin_type_validation_tests;
#[cfg(test)]
mod saved_value_retyped_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod value_json_schema_tests;
