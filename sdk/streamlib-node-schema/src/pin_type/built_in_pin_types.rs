// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use super::{PinFlow, PinType};

/// Declares [`BuiltInPinTypeName`] and its [`BuiltInPinTypeName::EVERY_NAME`] from one list, so
/// a name cannot be declared without being listed.
macro_rules! built_in_pin_type_names {
    ($($name:ident),+ $(,)?) => {
        /// The name of each built-in pin type: the `type` tag of its [`PinType`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum BuiltInPinTypeName {
            $($name),+
        }

        impl BuiltInPinTypeName {
            /// Every name, in the order the contract lists the rows.
            pub const EVERY_NAME: [Self; [$(Self::$name),+].len()] = [$(Self::$name),+];
        }
    };
}

built_in_pin_type_names![
    Boolean,
    Integer,
    Float,
    String,
    Menu,
    Color,
    Vector,
    Rectangle,
    Transform,
    Texture,
    Audio,
    EncodedVideo,
    EncodedAudio,
    Tensor,
    Asset,
    Trigger,
    ListOf,
    Bundle,
    Named,
    NodeReference,
    PinTypeChoice,
];

impl PinType {
    /// The name of the built-in type this is.
    pub fn built_in_name(&self) -> BuiltInPinTypeName {
        match self {
            Self::Boolean {} => BuiltInPinTypeName::Boolean,
            Self::Integer { .. } => BuiltInPinTypeName::Integer,
            Self::Float { .. } => BuiltInPinTypeName::Float,
            Self::String {} => BuiltInPinTypeName::String,
            Self::Menu { .. } => BuiltInPinTypeName::Menu,
            Self::Color {} => BuiltInPinTypeName::Color,
            Self::Vector { .. } => BuiltInPinTypeName::Vector,
            Self::Rectangle {} => BuiltInPinTypeName::Rectangle,
            Self::Transform {} => BuiltInPinTypeName::Transform,
            Self::Texture { .. } => BuiltInPinTypeName::Texture,
            Self::Audio {} => BuiltInPinTypeName::Audio,
            Self::EncodedVideo { .. } => BuiltInPinTypeName::EncodedVideo,
            Self::EncodedAudio { .. } => BuiltInPinTypeName::EncodedAudio,
            Self::Tensor {} => BuiltInPinTypeName::Tensor,
            Self::Asset { .. } => BuiltInPinTypeName::Asset,
            Self::Trigger {} => BuiltInPinTypeName::Trigger,
            Self::ListOf { .. } => BuiltInPinTypeName::ListOf,
            Self::Bundle { .. } => BuiltInPinTypeName::Bundle,
            Self::Named { .. } => BuiltInPinTypeName::Named,
            Self::NodeReference { .. } => BuiltInPinTypeName::NodeReference,
            Self::PinTypeChoice { .. } => BuiltInPinTypeName::PinTypeChoice,
        }
    }

    /// Whether a link may end at a pin of this type; the rows of the two that never may say so.
    pub fn takes_a_wire(&self) -> bool {
        !matches!(
            self,
            Self::NodeReference { .. } | Self::PinTypeChoice { .. }
        )
    }

    /// Whether this is a Boolean, an Integer or a Float, each a message of one number.
    pub(crate) fn is_a_number(&self) -> bool {
        matches!(
            self,
            Self::Boolean {} | Self::Integer { .. } | Self::Float { .. }
        )
    }
}

/// An annotation a pin of a type may state, by its key in [`super::PinAnnotations`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinAnnotationName {
    Range,
    Step,
    Unit,
    Meaning,
    Control,
    FrameRate,
    SampleRate,
    SampleType,
    Residency,
    ProjectAsset,
}

/// One built-in pin type, as the contract states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinTypeDescription {
    /// Its name.
    pub name: BuiltInPinTypeName,
    /// The words a person reads for it.
    pub label: &'static str,
    /// What it carries, in a sentence or two.
    pub documentation: &'static str,
    /// The flows a pin of it may state, its default first.
    pub flows: &'static [PinFlow],
    /// The annotations a pin of it may state.
    pub annotations: &'static [PinAnnotationName],
    /// Each way a Python declaration writes it, as the authoring surface spells it.
    pub python_forms: &'static [&'static str],
    /// How the TypeScript SDK writes it: its name as a string literal.
    pub typescript_form: &'static str,
}

use PinAnnotationName::{
    Control, FrameRate, Meaning, ProjectAsset, Range, Residency, SampleRate, SampleType, Step, Unit,
};

const VALUE_THEN_STREAM: &[PinFlow] = &[PinFlow::Value, PinFlow::Stream];
const STREAM_THEN_VALUE: &[PinFlow] = &[PinFlow::Stream, PinFlow::Value];
const STREAM_ONLY: &[PinFlow] = &[PinFlow::Stream];
const VALUE_ONLY: &[PinFlow] = &[PinFlow::Value];
const TRIGGER_ONLY: &[PinFlow] = &[PinFlow::Trigger];

const NUMBER_ANNOTATIONS: &[PinAnnotationName] = &[Range, Step, Unit, Meaning, Control];
const VALUE_ANNOTATIONS: &[PinAnnotationName] = &[Meaning, Control];
const STRING_ANNOTATIONS: &[PinAnnotationName] = &[Meaning, Control, ProjectAsset];
const MEANING_ONLY: &[PinAnnotationName] = &[Meaning];

/// One row per built-in pin type, in [`BuiltInPinTypeName::EVERY_NAME`] order.
pub fn built_in_pin_types() -> [PinTypeDescription; BuiltInPinTypeName::EVERY_NAME.len()] {
    BuiltInPinTypeName::EVERY_NAME.map(built_in_pin_type)
}

/// The row of one built-in pin type.
pub fn built_in_pin_type(name: BuiltInPinTypeName) -> PinTypeDescription {
    let row = |label,
               documentation,
               flows,
               annotations,
               python_forms: &'static [&'static str],
               typescript_form| {
        PinTypeDescription {
            name,
            label,
            documentation,
            flows,
            annotations,
            python_forms,
            typescript_form,
        }
    };
    match name {
        BuiltInPinTypeName::Boolean => row(
            "Boolean",
            "True or false.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["bool"],
            "\"boolean\"",
        ),
        BuiltInPinTypeName::Integer => row(
            "Integer",
            "A whole number, exact to 2^53, or within the exact size the type states.",
            VALUE_THEN_STREAM,
            NUMBER_ANNOTATIONS,
            &[
                "int", "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64",
            ],
            "\"integer\"",
        ),
        BuiltInPinTypeName::Float => row(
            "Float",
            "A number, held as a 64-bit float unless the type states a size.",
            VALUE_THEN_STREAM,
            NUMBER_ANNOTATIONS,
            &["float", "Float32", "Float64"],
            "\"float\"",
        ),
        BuiltInPinTypeName::String => row(
            "String",
            "Text.",
            VALUE_THEN_STREAM,
            STRING_ANNOTATIONS,
            &["str"],
            "\"string\"",
        ),
        BuiltInPinTypeName::Menu => row(
            "Menu",
            "One of a fixed list of option names.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["Literal"],
            "\"menu\"",
        ),
        BuiltInPinTypeName::Color => row(
            "Color",
            "An sRGB colour with alpha, four numbers from 0 to 1.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["Color"],
            "\"color\"",
        ),
        BuiltInPinTypeName::Vector => row(
            "Vector",
            "Two, three or four numbers, named x, y, z and w.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["Vector2", "Vector3", "Vector4"],
            "\"vector\"",
        ),
        BuiltInPinTypeName::Rectangle => row(
            "Rectangle",
            "A rectangle by its left and top edges, width and height.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["Rectangle"],
            "\"rectangle\"",
        ),
        BuiltInPinTypeName::Transform => row(
            "Transform",
            "A 4 by 4 matrix, row by row.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["Transform"],
            "\"transform\"",
        ),
        BuiltInPinTypeName::Texture => row(
            "Texture",
            "A GPU picture, in one of the pixel formats the type states.",
            STREAM_THEN_VALUE,
            &[FrameRate, Meaning, Control],
            &["Texture"],
            "\"texture\"",
        ),
        BuiltInPinTypeName::Audio => row(
            "Audio",
            "A stream of audio samples.",
            STREAM_ONLY,
            &[SampleRate, SampleType, Meaning, Control],
            &["Audio"],
            "\"audio\"",
        ),
        BuiltInPinTypeName::EncodedVideo => row(
            "Encoded video",
            "Encoded video, one access unit per message, in one of the codecs the type states.",
            STREAM_ONLY,
            MEANING_ONLY,
            &["EncodedVideo"],
            "\"encoded_video\"",
        ),
        BuiltInPinTypeName::EncodedAudio => row(
            "Encoded audio",
            "Encoded audio, one packet per message, in one of the codecs the type states.",
            STREAM_ONLY,
            MEANING_ONLY,
            &["EncodedAudio"],
            "\"encoded_audio\"",
        ),
        BuiltInPinTypeName::Tensor => row(
            "Tensor",
            "An array of numbers with a shape.",
            STREAM_THEN_VALUE,
            &[Residency, Meaning],
            &["Tensor"],
            "\"tensor\"",
        ),
        BuiltInPinTypeName::Asset => row(
            "Asset",
            "One of the project's assets, of the media the type states.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["Asset"],
            "\"asset\"",
        ),
        BuiltInPinTypeName::Trigger => row(
            "Trigger",
            "An event with no payload; its message's timestamp is the event.",
            TRIGGER_ONLY,
            VALUE_ANNOTATIONS,
            &["Trigger"],
            "\"trigger\"",
        ),
        BuiltInPinTypeName::ListOf => row(
            "List",
            "A list of values of the one type it names.",
            VALUE_THEN_STREAM,
            VALUE_ANNOTATIONS,
            &["list"],
            "\"list_of\"",
        ),
        BuiltInPinTypeName::Bundle => row(
            "Bundle",
            "Named members, each of its own type, carried as one message. It has no name of its \
             own: two bundles with the same members are one type.",
            VALUE_THEN_STREAM,
            MEANING_ONLY,
            &["dataclass"],
            "\"bundle\"",
        ),
        BuiltInPinTypeName::Named => row(
            "Named type",
            "A type a module declares by its class, carried as the bytes that class packs.",
            STREAM_THEN_VALUE,
            MEANING_ONLY,
            &["named_pin_type"],
            "\"named\"",
        ),
        BuiltInPinTypeName::NodeReference => row(
            "Node reference",
            "A setting naming another node of the graph. It never takes a wire.",
            VALUE_ONLY,
            &[],
            &[
                "NodeReference",
                "NodeReferenceToNodeType",
                "SoleReferrerNodeReferenceToNodeType",
            ],
            "\"node_reference\"",
        ),
        BuiltInPinTypeName::PinTypeChoice => row(
            "Pin type choice",
            "A setting that chooses another port's pin type; its value is a pin type. It never \
             takes a wire.",
            VALUE_ONLY,
            &[],
            &["PinTypeChoice"],
            "\"pin_type_choice\"",
        ),
    }
}

#[cfg(feature = "contract-json-schema")]
impl BuiltInPinTypeName {
    /// The JSON Schema of the type's stored value, its named types added to `generator`'s
    /// definitions; `None` for a type that stores no value.
    pub fn value_json_schema(
        self,
        generator: &mut schemars_1::SchemaGenerator,
    ) -> Option<schemars_1::Schema> {
        use super::stored_pin_values::{
            AssetPinValue, BooleanPinValue, BundlePinValue, ColorPinValue, FloatPinValue,
            IntegerPinValue, ListPinValue, MenuPinValue, RectanglePinValue, StringPinValue,
            TransformPinValue, VectorPinValue,
        };
        match self {
            Self::Boolean => Some(generator.subschema_for::<BooleanPinValue>()),
            Self::Integer => Some(generator.subschema_for::<IntegerPinValue>()),
            Self::Float => Some(generator.subschema_for::<FloatPinValue>()),
            Self::String => Some(generator.subschema_for::<StringPinValue>()),
            Self::Menu => Some(generator.subschema_for::<MenuPinValue>()),
            Self::Color => Some(generator.subschema_for::<ColorPinValue>()),
            Self::Vector => Some(generator.subschema_for::<VectorPinValue>()),
            Self::Rectangle => Some(generator.subschema_for::<RectanglePinValue>()),
            Self::Transform => Some(generator.subschema_for::<TransformPinValue>()),
            Self::Asset => Some(generator.subschema_for::<AssetPinValue>()),
            Self::ListOf => Some(generator.subschema_for::<ListPinValue>()),
            Self::Bundle => Some(generator.subschema_for::<BundlePinValue>()),
            Self::NodeReference => Some(generator.subschema_for::<crate::NodeReferenceInConfig>()),
            Self::PinTypeChoice => Some(generator.subschema_for::<PinType>()),
            Self::Texture
            | Self::Audio
            | Self::EncodedVideo
            | Self::EncodedAudio
            | Self::Tensor
            | Self::Trigger
            | Self::Named => None,
        }
    }
}

/// A closed set of names, as the contract writes one: a `oneOf` of string `const`s, each
/// described by its row's documentation.
#[cfg(feature = "contract-json-schema")]
impl schemars_1::JsonSchema for BuiltInPinTypeName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "BuiltInPinTypeName".into()
    }

    fn json_schema(_generator: &mut schemars_1::SchemaGenerator) -> schemars_1::Schema {
        let names: Vec<serde_json::Value> = Self::EVERY_NAME
            .iter()
            .map(|name| {
                serde_json::json!({
                    "type": "string",
                    "const": name,
                    "description": built_in_pin_type(*name).documentation,
                })
            })
            .collect();
        schemars_1::json_schema!({
            "oneOf": names,
            "description": "The name of a built-in pin type: the `type` tag of its pin type.",
        })
    }
}
