// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a port carries: the closed set of pin types, what a port declares, the JSON form of each
//! type's stored value, and one row per built-in type for the control contract. Behind the
//! `contract-json-schema` feature each type derives the schemars the contract renders.

mod built_in_pin_types;
mod pin_declaration;
mod pin_type_variants;
mod stored_pin_values;

pub use built_in_pin_types::{
    BuiltInPinTypeName, PinAnnotationName, PinTypeDescription, built_in_pin_type,
    built_in_pin_types,
};
pub use pin_declaration::{
    DeclaredPinType, NodeInputMode, PinAnnotations, PinDeclaration, PinFlow, PinNumberRange,
    SettingReadTime,
};
pub use pin_type_variants::{
    BundleMember, FloatSize, IntegerSize, NamedPinTypeImportPath, PinType, ProjectAssetMedia,
    TexturePixelFormat, VectorComponentCount,
};
pub use stored_pin_values::{
    AssetPinValue, BooleanPinValue, BundlePinValue, ColorPinValue, FloatPinValue, IntegerPinValue,
    ListPinValue, MenuPinValue, RectanglePinValue, StringPinValue, TransformPinValue,
    VectorPinValue,
};

#[cfg(test)]
mod tests;
