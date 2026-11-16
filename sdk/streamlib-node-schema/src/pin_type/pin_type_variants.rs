// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Deserializer, Serialize};

use crate::NodeClassImportPath;
use crate::error::{SchemaError, SchemaResult};

/// What a port carries, as an author, a person and a client name it.
///
/// The set is closed: a named type and a bundle are how a project adds its own.
// A type with nothing to state is an empty struct variant: serde ignores the keys beside the tag
// of an internally tagged unit variant, and refuses them on a struct variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PinType {
    /// True or false.
    Boolean {},
    /// A whole number.
    Integer {
        /// The size the number must fit; absent, any whole number exact to 2^53.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact_size: Option<IntegerSize>,
    },
    /// A number.
    Float {
        /// The size the number is held in; absent, a 64-bit float.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact_size: Option<FloatSize>,
    },
    /// Text.
    String {},
    /// One of a fixed list of option names.
    Menu {
        /// The options, spelled as declared.
        option_names: Vec<String>,
    },
    /// An sRGB colour with alpha.
    Color {},
    /// Two, three or four numbers.
    Vector {
        /// How many numbers it holds.
        component_count: VectorComponentCount,
    },
    /// A rectangle by its left and top edges, width and height.
    Rectangle {},
    /// A 4 by 4 matrix.
    Transform {},
    /// A GPU picture.
    Texture {
        /// The formats it may hold; on an input, empty takes any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pixel_formats: Vec<TexturePixelFormat>,
    },
    /// A stream of audio samples.
    Audio {},
    /// Encoded video, one access unit per message.
    EncodedVideo {
        /// The codecs it may hold; empty takes any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        codecs: Vec<String>,
    },
    /// Encoded audio, one packet per message.
    EncodedAudio {
        /// The codecs it may hold; empty takes any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        codecs: Vec<String>,
    },
    /// An array of numbers with a shape.
    Tensor {},
    /// One of the project's assets.
    Asset {
        /// The media the asset must hold one of; empty takes any asset.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media: Vec<ProjectAssetMedia>,
    },
    /// An event with no payload.
    Trigger {},
    /// A list of values of one type.
    ListOf {
        /// The type of each element.
        element_type: Box<PinType>,
    },
    /// Named members, each of its own type, carried as one.
    Bundle {
        /// The members, in declared order.
        members: Vec<BundleMember>,
    },
    /// A type a module declares by its class.
    Named {
        /// The class that declares it.
        named_pin_type: NamedPinTypeImportPath,
    },
    /// A setting naming another node of the graph. It never takes a wire.
    NodeReference {
        /// The node type the referenced node must be; absent, any node.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "contract-json-schema", schemars(with = "Option<String>"))]
        referenced_node_type: Option<NodeClassImportPath>,
        /// Whether one node at most may name a given node through this setting.
        #[serde(default)]
        one_referrer_per_referenced_node: bool,
    },
    /// A setting that chooses another port's pin type; its value is a pin type. It never takes
    /// a wire.
    PinTypeChoice {
        /// The types it may choose; empty chooses any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        among: Vec<PinType>,
    },
}

/// The size an Integer must fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum IntegerSize {
    /// A signed 8-bit integer.
    Int8,
    /// A signed 16-bit integer.
    Int16,
    /// A signed 32-bit integer.
    Int32,
    /// A signed 64-bit integer, as a Tensor or List element only.
    Int64,
    /// An unsigned 8-bit integer.
    #[serde(rename = "uint8")]
    UInt8,
    /// An unsigned 16-bit integer.
    #[serde(rename = "uint16")]
    UInt16,
    /// An unsigned 32-bit integer.
    #[serde(rename = "uint32")]
    UInt32,
    /// An unsigned 64-bit integer, as a Tensor or List element only.
    #[serde(rename = "uint64")]
    UInt64,
}

/// The size a Float is held in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum FloatSize {
    /// A 32-bit float.
    Float32,
    /// A 64-bit float.
    Float64,
}

/// How many numbers a Vector holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum VectorComponentCount {
    /// `x` and `y`.
    Two,
    /// `x`, `y` and `z`.
    Three,
    /// `x`, `y`, `z` and `w`.
    Four,
}

/// Declares [`TexturePixelFormat`], its [`TexturePixelFormat::EVERY_FORMAT`] and its
/// [`TexturePixelFormat::wire_name`] from one list, so a format cannot be declared without being
/// listed, and it serializes as the wire name it is listed with.
macro_rules! texture_pixel_formats {
    ($($(#[$attribute:meta])* $format:ident => $wire_name:literal),+ $(,)?) => {
        /// A texture format, spelled as the escalate wire spells it.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(
            feature = "contract-json-schema",
            derive(schemars_1::JsonSchema),
            schemars(crate = "schemars_1")
        )]
        pub enum TexturePixelFormat {
            $(
                $(#[$attribute])*
                #[serde(rename = $wire_name)]
                $format,
            )+
        }

        impl TexturePixelFormat {
            /// Every format, in declaration order.
            pub const EVERY_FORMAT: [Self; [$(Self::$format),+].len()] = [$(Self::$format),+];

            /// The format's name on the escalate wire, as it serializes.
            pub const fn wire_name(self) -> &'static str {
                match self {
                    $(Self::$format => $wire_name,)+
                }
            }
        }
    };
}

texture_pixel_formats![
    /// Four 8-bit channels, red first, read as 0 to 1.
    Rgba8Unorm => "rgba8_unorm",
    /// Four 8-bit channels, red first, sRGB-encoded.
    Rgba8UnormSrgb => "rgba8_unorm_srgb",
    /// Four 8-bit channels, blue first, read as 0 to 1.
    Bgra8Unorm => "bgra8_unorm",
    /// Four 8-bit channels, blue first, sRGB-encoded.
    Bgra8UnormSrgb => "bgra8_unorm_srgb",
    /// Four 16-bit float channels.
    Rgba16Float => "rgba16_float",
    /// Four 32-bit float channels.
    Rgba32Float => "rgba32_float",
    /// 8-bit luma, then interleaved half-size chroma.
    Nv12 => "nv12",
];

/// What an asset holds, as its media facts or its kind name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAssetMedia {
    /// A still picture.
    Image,
    /// Moving pictures.
    Video,
    /// Sound.
    Audio,
    /// A colour lookup table.
    Lut,
    /// A 3D scene.
    Scene,
    /// A stream's graph document, matched against the asset's kind.
    Stream,
}

/// One member of a bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct BundleMember {
    /// The member's name, a key of the bundle's stored value.
    pub name: String,
    /// The member's type.
    pub member_type: PinType,
    /// What the member's value stands for, such as `frequency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meaning: Option<String>,
    /// The value a stored bundle that leaves the member out reads; absent, the member must be
    /// present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
}

/// The class that declares a named pin type, `module:QualifiedClassName`.
// Every inhabitant names one: the inner string is private, `new` is the only constructor, and
// `Deserialize` validates rather than inheriting `String`'s.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(transparent)]
pub struct NamedPinTypeImportPath(String);

impl NamedPinTypeImportPath {
    /// Build a path, refusing one that is blank or has no `:` between its module and its
    /// class.
    pub fn new(import_path: impl Into<String>) -> SchemaResult<Self> {
        let import_path = import_path.into();
        let names_a_class =
            import_path
                .split_once(':')
                .is_some_and(|(module_name, qualified_class_name)| {
                    !module_name.trim().is_empty() && !qualified_class_name.trim().is_empty()
                });
        if !names_a_class {
            return Err(SchemaError::InvalidName {
                name: import_path,
                reason: "a named pin type is the class that declares it, written \
                         `module:QualifiedClassName`; this one names no module and class"
                    .to_string(),
            });
        }
        Ok(Self(import_path))
    }

    /// The path as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for NamedPinTypeImportPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}
