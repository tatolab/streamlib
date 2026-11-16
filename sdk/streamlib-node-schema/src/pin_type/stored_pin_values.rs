// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The JSON form of each built-in pin type's stored value, as a document saves it and a client
//! sets and reads it. A schema here cannot say what depends on the pin type: an Integer's exact
//! size, a Menu's options, a Vector's declared size, a List's element and a bundle's members. A
//! node reference's stored value is [`crate::NodeReferenceInConfig`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A Boolean's stored value: a JSON boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct BooleanPinValue(pub bool);

/// An Integer's stored value: a whole number, within its exact size when the type states one
/// and exact to 2^53 when it states none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct IntegerPinValue(pub i64);

/// A Float's stored value: any JSON number; `1` is taken as 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct FloatPinValue(pub f64);

/// A String's stored value: any text; the empty string is a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct StringPinValue(pub String);

/// A Menu's stored value: one of the type's option names, spelled as declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct MenuPinValue(pub String);

/// A Color's stored value: sRGB red, green and blue and alpha, each 0 for none and 1 for full,
/// not linear light and not multiplied by alpha. Alpha 1 is opaque. A number outside 0 to 1 is
/// taken as sent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct ColorPinValue {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

/// A Vector's stored value: `x` and `y`, with `z` for three components and `w` for four, and no
/// other key. The type's declared size says which keys it holds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct VectorPinValue {
    pub x: f64,
    pub y: f64,
    /// Present for three and four components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "contract-json-schema", schemars(with = "f64"))]
    pub z: Option<f64>,
    /// Present for four components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "contract-json-schema", schemars(with = "f64"))]
    pub w: Option<f64>,
}

/// A Rectangle's stored value: its left edge `x`, its top edge `y`, and a `width` and `height`
/// of 0 or more. The node says whether they are pixels or a share of the frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct RectanglePinValue {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A Transform's stored value: a 4 by 4 matrix as 16 numbers, row by row. It multiplies a
/// column, so the fourth number of each of the first three rows is the translation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct TransformPinValue(pub [f64; 16]);

/// An Asset's stored value: the asset's UUID as `list_assets` answers it, with `from`, the
/// import's project UUID, for an asset of an import. Never a bare string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct AssetPinValue {
    /// The asset's UUID.
    pub asset: Uuid,
    /// The UUID of the imported project that holds the asset; absent, the project's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "contract-json-schema", schemars(with = "Uuid"))]
    pub from: Option<Uuid>,
}

/// A List's stored value: a list of its element's form, in order. `[]` is a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct ListPinValue(pub Vec<serde_json::Value>);

/// A bundle's stored value: an object keyed by member name, each member in its own type's form.
/// Only a member that declares a default may be left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct BundlePinValue(pub serde_json::Map<String, serde_json::Value>);
