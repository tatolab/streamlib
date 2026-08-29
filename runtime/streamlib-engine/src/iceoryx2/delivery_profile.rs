// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The single per-port delivery knob on the authoring surface.
//!
//! A [`DeliveryProfile`] is the one word an author writes at a port declaration
//! site (`#[processor]` attribute / `@processor` decorator). It names a read
//! policy — which bag the consumer gets next — and resolves to the
//! consumer-side drain order ([`ReadMode`]), the producer-side overflow policy
//! ([`Overflow`]), and the ring depth. Every input port declares one and
//! nothing is inferred — an input port without a profile is a wiring error.

use streamlib_processor_schema::DELIVERY_PROFILE_DECLARATION_VALUES;
use streamlib_processor_schema::ProcessorClassImportPath;

use super::overflow::Overflow;
use super::read_mode::ReadMode;
use crate::core::error::{Error, Result};
use crate::core::processors::PROCESSOR_REGISTRY;

/// The legal `delivery_profile` values as a quoted, comma-joined list.
fn render_delivery_profile_values() -> String {
    DELIVERY_PROFILE_DECLARATION_VALUES
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one per-port delivery knob. Each profile bundles a fixed
/// (drain order, overflow policy, ring depth) triple; see [`DeliveryProfile::resolve`].
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

/// The resolved transport triple a [`DeliveryProfile`] expands to at wire time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryResolution {
    /// Consumer-side drain order applied by the destination's mailbox.
    pub drain_order: ReadMode,
    /// Producer-side overflow policy sizing the channel's `enable_safe_overflow`.
    pub overflow: Overflow,
    /// Ring depth — both the iceoryx2 subscriber buffer and the host mailbox
    /// capacity.
    pub depth: usize,
}

impl DeliveryProfile {
    /// Ring depth for [`DeliveryProfile::Newest`].
    pub const NEWEST_DEPTH: usize = 4;
    /// Ring depth for [`DeliveryProfile::Ordered`].
    pub const ORDERED_DEPTH: usize = 16;

    /// Expand this profile into its fixed (drain order, overflow, depth) triple.
    pub fn resolve(self) -> DeliveryResolution {
        match self {
            DeliveryProfile::Newest => DeliveryResolution {
                drain_order: ReadMode::SkipToLatest,
                overflow: Overflow::DropOldest,
                depth: Self::NEWEST_DEPTH,
            },
            DeliveryProfile::Ordered => DeliveryResolution {
                drain_order: ReadMode::ReadNextInOrder,
                overflow: Overflow::DropOldest,
                depth: Self::ORDERED_DEPTH,
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

/// Resolve the [`DeliveryProfile`] a destination input port declares.
///
/// This is the single delivery-resolution primitive, and it reads exactly one
/// thing: the port's own declaration. Nothing is inferred, so a registered
/// input port carrying no declaration is a wiring error, not a silent
/// substitution.
///
/// Falls back to [`DeliveryProfile::Newest`] when the destination processor
/// type isn't registered or the named port doesn't exist (defensive; a Wired
/// link always resolves both — the wiring path itself reports the missing
/// processor).
pub(crate) fn delivery_profile_for_input_port(
    processor_type: &ProcessorClassImportPath,
    port_name: &str,
) -> Result<DeliveryProfile> {
    let Some((inputs, _outputs)) = PROCESSOR_REGISTRY.port_info(processor_type) else {
        return Ok(DeliveryProfile::Newest);
    };
    let Some(port) = inputs.iter().find(|p| p.name == port_name) else {
        return Ok(DeliveryProfile::Newest);
    };

    let Some(declared) = port.delivery_profile.as_deref() else {
        return Err(Error::Configuration(format!(
            "input port '{port_name}' on '{processor_type}' declares no delivery_profile. \
             Every input port must declare one — {}. There is no default: channel policy \
             is declared port-locally at the consuming input port",
            render_delivery_profile_values()
        )));
    };
    DeliveryProfile::from_manifest_str(declared).map_err(|err| {
        Error::Configuration(format!(
            "input port '{port_name}' on '{processor_type}' declared {err}"
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
        assert_eq!(resolution.overflow, Overflow::DropOldest);
        assert_eq!(resolution.depth, 4);
        assert!(resolution.overflow.enable_safe_overflow());
    }

    #[test]
    fn ordered_resolves_to_fifo_drop_deep() {
        let resolution = DeliveryProfile::Ordered.resolve();
        assert_eq!(resolution.drain_order, ReadMode::ReadNextInOrder);
        assert_eq!(resolution.overflow, Overflow::DropOldest);
        assert_eq!(resolution.depth, 16);
        assert!(resolution.overflow.enable_safe_overflow());
    }

    /// No declarable profile reaches producer-blocking. Driven from the
    /// declaration constant rather than a literal so a profile added there is
    /// covered here without anyone remembering to widen this.
    #[test]
    fn no_declarable_profile_resolves_to_a_blocking_producer() {
        for declared_value in DELIVERY_PROFILE_DECLARATION_VALUES {
            let resolution = DeliveryProfile::from_manifest_str(declared_value)
                .expect("every legal declaration value parses")
                .resolve();
            assert_eq!(
                resolution.overflow,
                Overflow::DropOldest,
                "'{declared_value}' must drop rather than park its producer"
            );
            assert!(resolution.overflow.enable_safe_overflow());
        }
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
        //! `delivery_profile_for_input_port` reads the port's own declaration
        //! and nothing else. There is no second source to fall back to.

        use super::super::{DeliveryProfile, delivery_profile_for_input_port};
        use crate::core::descriptors::{
            PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
        };
        use crate::core::error::Error;
        use crate::core::processors::PROCESSOR_REGISTRY;

        fn class_path(type_name: &str) -> ProcessorClassImportPath {
            ProcessorClassImportPath::new(format!("{}::{type_name}", module_path!())).unwrap()
        }

        /// Registers a processor carrying one input port, optionally declaring a
        /// delivery profile, and returns the processor's class import path.
        fn register_processor_with_one_input_port(
            type_name: &str,
            port_name: &str,
            declared_profile: Option<&str>,
        ) -> ProcessorClassImportPath {
            let import_path = class_path(type_name);
            let mut port = PortDescriptor::iceoryx2(port_name, "input");
            if let Some(profile) = declared_profile {
                port = port.with_delivery_profile(profile);
            }
            let mut descriptor = ProcessorDescriptor::new(
                ProcessorClassShortName::new(type_name).unwrap(),
                import_path.clone(),
                type_name,
            );
            descriptor.inputs.push(port);
            PROCESSOR_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("descriptor registration");
            import_path
        }

        /// The default-fallback path: an unregistered processor type yields
        /// the realtime default, the shallowest ring there is. Mentally
        /// reverting this to the deeper profile would hand a defensively
        /// handled case a backlog nothing asked for.
        #[test]
        fn unregistered_processor_falls_back_to_newest() {
            let unknown = class_path("NothingRegisteredUnderThisPath");
            assert_eq!(
                delivery_profile_for_input_port(&unknown, "video_in").unwrap(),
                DeliveryProfile::Newest
            );
        }

        #[test]
        fn declared_profile_is_the_whole_answer() {
            let ident =
                register_processor_with_one_input_port("OrderedSink", "video_in", Some("ordered"));
            assert_eq!(
                delivery_profile_for_input_port(&ident, "video_in").unwrap(),
                DeliveryProfile::Ordered,
            );
        }

        /// A registered input port carrying no declaration is a wiring error
        /// naming the port. There is nothing left to infer a profile from, so
        /// any resolution here other than an error is a regression.
        #[test]
        fn missing_declaration_is_a_wiring_error_naming_the_port() {
            let ident = register_processor_with_one_input_port("SampleSink", "audio_in", None);
            let err = delivery_profile_for_input_port(&ident, "audio_in")
                .expect_err("an undeclared delivery profile must be a wiring error");
            let msg = err.to_string();
            assert!(
                msg.contains("audio_in"),
                "the error must name the port: {msg}"
            );
            assert!(msg.contains("declares no delivery_profile"), "got: {msg}");
            assert!(matches!(err, Error::Configuration(_)));
        }

        /// A typo at the declaration site surfaces as a typed configuration
        /// error listing the legal values, never a silent default.
        #[test]
        fn unknown_declared_value_is_rejected_with_the_legal_values() {
            let ident = register_processor_with_one_input_port(
                "TypoSink",
                "video_in",
                Some("skip_to_latest"), // retired knob, not a profile
            );
            let err = delivery_profile_for_input_port(&ident, "video_in")
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
