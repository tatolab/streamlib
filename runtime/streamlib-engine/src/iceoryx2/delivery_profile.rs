// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_node_schema::NodeClassImportPath;
use streamlib_node_schema::{DEFAULT_INPUT_PORT_BORROW_QUOTA, DELIVERY_PROFILE_DECLARATION_VALUES};

use super::read_mode::ReadMode;
use crate::core::error::{Error, Result};
use crate::core::nodes::NodeTypesOneStreamResolves;

/// The legal `delivery_profile` values as a quoted, comma-joined list.
fn render_delivery_profile_values() -> String {
    DELIVERY_PROFILE_DECLARATION_VALUES
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one per-port delivery knob. Each profile bundles a fixed
/// (drain order, ring depth) pair; see [`DeliveryProfile::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryProfile {
    /// The consumer drains to the most recent bag; older ones are passed
    /// over. Shallow ring. State snapshots — video frames, control state —
    /// where a stale bag has no value once a fresher one exists.
    Newest,
    /// The consumer receives bags in publication order, evicting the oldest
    /// under sustained overrun. Deeper ring. Sample streams — audio blocks,
    /// encoded frames — where order carries meaning.
    Ordered,
}

/// How one input port's deliveries are handled, resolved at wire time from the
/// [`DeliveryProfile`] and borrow quota it declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryResolution {
    /// Consumer-side drain order applied by the destination's mailbox.
    pub drain_order: ReadMode,
    /// Ring depth — both the iceoryx2 subscriber buffer and the mailbox capacity
    /// of an input port that declares no window contract. A windowed port's
    /// subscriber takes the engine's fixed windowed ring and its mailbox is
    /// sized from its contract.
    pub depth: usize,
    /// The samples each inbound link's reader may hold beyond the mailbox.
    pub borrow_quota: usize,
}

impl DeliveryProfile {
    /// Ring depth for [`DeliveryProfile::Newest`].
    pub const NEWEST_DEPTH: usize = 4;
    /// Ring depth for [`DeliveryProfile::Ordered`].
    pub const ORDERED_DEPTH: usize = 16;

    /// Expand this profile into its fixed drain order and depth, under the
    /// default borrow quota.
    pub fn resolve(self) -> DeliveryResolution {
        match self {
            DeliveryProfile::Newest => DeliveryResolution {
                drain_order: ReadMode::SkipToLatest,
                depth: Self::NEWEST_DEPTH,
                borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
            },
            DeliveryProfile::Ordered => DeliveryResolution {
                drain_order: ReadMode::ReadNextInOrder,
                depth: Self::ORDERED_DEPTH,
                borrow_quota: DEFAULT_INPUT_PORT_BORROW_QUOTA,
            },
        }
    }

    /// Parse an author-declared profile string.
    ///
    /// Recognized values: `"newest"`, `"ordered"`. Unknown values surface as a
    /// structured configuration error so a typo at the declaration site is
    /// rejected at wire time, not silently defaulted.
    pub fn from_manifest_str(value: &str) -> std::result::Result<Self, String> {
        match value {
            "newest" => Ok(Self::Newest),
            "ordered" => Ok(Self::Ordered),
            other => Err(format!(
                "unknown delivery_profile value '{other}', expected one of {}",
                render_delivery_profile_values()
            )),
        }
    }

    /// The canonical manifest string for this profile.
    pub fn as_manifest_str(self) -> &'static str {
        match self {
            DeliveryProfile::Newest => "newest",
            DeliveryProfile::Ordered => "ordered",
        }
    }
}

pub(crate) fn delivery_profile_for_input_port(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
    port_name: &str,
) -> Result<DeliveryProfile> {
    let Some(ports) = node_types.port_info(node_type) else {
        return Ok(DeliveryProfile::Newest);
    };
    let Some(port) = ports.the_input_of_the_type_a_name_finds(port_name) else {
        return Ok(DeliveryProfile::Newest);
    };

    let Some(declared) = port.delivery_profile.as_deref() else {
        return Err(Error::Configuration(format!(
            "input port '{port_name}' on '{node_type}' reads by no delivery_profile: it \
             declares none and has no pin to default one from. Declare one — {}",
            render_delivery_profile_values()
        )));
    };
    DeliveryProfile::from_manifest_str(declared).map_err(|err| {
        Error::Configuration(format!(
            "input port '{port_name}' on '{node_type}' declared {err}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_resolves_to_skip_drop_shallow() {
        let resolution = DeliveryProfile::Newest.resolve();
        assert_eq!(resolution.drain_order, ReadMode::SkipToLatest);
        assert_eq!(resolution.depth, 4);
    }

    #[test]
    fn ordered_resolves_to_fifo_drop_deep() {
        let resolution = DeliveryProfile::Ordered.resolve();
        assert_eq!(resolution.drain_order, ReadMode::ReadNextInOrder);
        assert_eq!(resolution.depth, 16);
    }

    #[test]
    fn profile_parses_known_and_rejects_unknown() {
        assert_eq!(
            DeliveryProfile::from_manifest_str("newest").unwrap(),
            DeliveryProfile::Newest
        );
        assert_eq!(
            DeliveryProfile::from_manifest_str("ordered").unwrap(),
            DeliveryProfile::Ordered
        );
        // Case is part of the value: the declaration surface is lowercase, so
        // a capitalised spelling is a typo like any other.
        let err = DeliveryProfile::from_manifest_str("Newest").unwrap_err();
        assert!(
            err.contains("'newest'") && err.contains("'ordered'"),
            "{err}"
        );
    }

    /// The parser and the declaration constant agree in both directions: every
    /// legal value parses, and every parsed profile renders back the same
    /// spelling. A value in the constant that the parser rejects would make
    /// every error message list a word the engine refuses.
    #[test]
    fn manifest_str_roundtrips_through_the_declaration_constant() {
        for declared_value in DELIVERY_PROFILE_DECLARATION_VALUES {
            let profile = DeliveryProfile::from_manifest_str(declared_value)
                .expect("every legal declaration value parses");
            assert_eq!(profile.as_manifest_str(), declared_value);
        }
    }

    mod port_declaration_resolution {
        //! `delivery_profile_for_input_port` reads the port's own declaration,
        //! or its pin's default where it declares none.

        use super::super::{DeliveryProfile, delivery_profile_for_input_port};
        use crate::core::descriptors::{
            NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType,
            PortDescriptor,
        };
        use crate::core::error::Error;
        use crate::core::nodes::{NODE_REGISTRY, NodeTypesOneStreamResolves};

        fn class_path(type_name: &str) -> NodeClassImportPath {
            NodeClassImportPath::new(format!("{}::{type_name}", module_path!())).unwrap()
        }

        fn a_node_type_with_one_input_port(
            type_name: &str,
            port_name: &str,
            pin_type: Option<PinType>,
            declared_profile: Option<&str>,
        ) -> NodeDescriptor {
            let mut port = PortDescriptor {
                pin_declaration: pin_type.map(PinDeclaration::exactly),
                ..PortDescriptor::a_setting_of_no_pin_type(port_name, "input")
            };
            if let Some(profile) = declared_profile {
                port = port.with_delivery_profile(profile);
            }
            let mut descriptor = NodeDescriptor::new(
                NodeClassShortName::new(type_name).unwrap(),
                class_path(type_name),
                type_name,
            );
            descriptor.inputs.push(port);
            descriptor
        }

        fn register_node_with_one_input_port(
            type_name: &str,
            port_name: &str,
            declared_profile: Option<&str>,
        ) -> NodeClassImportPath {
            let descriptor = a_node_type_with_one_input_port(
                type_name,
                port_name,
                Some(PinType::Texture {
                    pixel_formats: Vec::new(),
                }),
                declared_profile,
            );
            let import_path = descriptor.node_class_import_path.clone();
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("descriptor registration");
            import_path
        }

        #[test]
        fn unregistered_node_falls_back_to_newest() {
            let unknown = class_path("NothingRegisteredUnderThisPath");
            assert_eq!(
                delivery_profile_for_input_port(
                    &NodeTypesOneStreamResolves::new(),
                    &unknown,
                    "video_in"
                )
                .unwrap(),
                DeliveryProfile::Newest
            );
        }

        #[test]
        fn declared_profile_is_the_whole_answer() {
            let ident =
                register_node_with_one_input_port("OrderedSink", "video_in", Some("ordered"));
            assert_eq!(
                delivery_profile_for_input_port(
                    &NodeTypesOneStreamResolves::new(),
                    &ident,
                    "video_in"
                )
                .unwrap(),
                DeliveryProfile::Ordered,
            );
        }

        /// A picture input that declares no profile reads by its Texture
        /// pin's default, `newest`, and the catalog serves that profile.
        #[test]
        fn a_typed_input_declaring_none_reads_by_its_pins_default() {
            let ident = register_node_with_one_input_port("PictureSink", "video_in", None);
            assert_eq!(
                delivery_profile_for_input_port(
                    &NodeTypesOneStreamResolves::new(),
                    &ident,
                    "video_in"
                )
                .unwrap(),
                DeliveryProfile::Newest
            );
            let served = NODE_REGISTRY.descriptor(&ident).unwrap();
            assert_eq!(served.inputs[0].delivery_profile.as_deref(), Some("newest"));
        }

        /// An input with no pin and no declared profile has nothing to read
        /// by, so wiring it is an error naming the port.
        #[test]
        fn an_untyped_input_declaring_none_is_a_wiring_error_naming_the_port() {
            let descriptor = a_node_type_with_one_input_port(
                "SampleSink",
                "records_in",
                None,
                None,
            );
            let ident = descriptor.node_class_import_path.clone();
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("descriptor registration");
            let err = delivery_profile_for_input_port(
                &NodeTypesOneStreamResolves::new(),
                &ident,
                "records_in",
            )
            .expect_err("an input with nothing to read by must be a wiring error");
            let msg = err.to_string();
            assert!(
                msg.contains("'records_in'") && msg.contains("reads by no delivery_profile"),
                "the error must name the port: {msg}"
            );
            assert!(matches!(err, Error::Configuration(_)));
        }

        /// A typo at the declaration site surfaces as a typed configuration
        /// error listing the legal values, never a silent default.
        #[test]
        fn unknown_declared_value_is_rejected_with_the_legal_values() {
            let ident = register_node_with_one_input_port(
                "TypoSink",
                "video_in",
                Some("skip_to_latest"), // retired knob, not a profile
            );
            let err = delivery_profile_for_input_port(
                &NodeTypesOneStreamResolves::new(),
                &ident,
                "video_in",
            )
            .expect_err("unknown delivery_profile must error");
            let msg = err.to_string();
            assert!(
                msg.contains("'newest'"),
                "error must list the legal values, quoted as the renderer emits them: {msg}"
            );
            assert!(
                msg.contains("'ordered'"),
                "error must list the legal values, quoted as the renderer emits them: {msg}"
            );
            assert!(matches!(err, Error::Configuration(_)));
        }
    }
}
