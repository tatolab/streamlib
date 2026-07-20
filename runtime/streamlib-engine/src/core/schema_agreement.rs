// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Producer/consumer schema-agreement checks for a wired link.
//!
//! Every frame already carries a 128-byte [`SchemaIdentWire`] tag stamped from
//! the producer's output [`PortSchemaSpec`]; this module is the single place
//! that reads the two ends and decides whether they agree. Two surfaces feed
//! it:
//!
//! - Connect-time: two [`PortSchemaSpec`]s (producer output vs consumer input),
//!   resolved from the registry before the link is wired.
//! - Runtime: two [`SchemaIdentWire`]s (the tag stamped on an inbound frame vs
//!   the consumer port's expected tag), compared per read.
//!
//! Agreement is intentionally permissive: an `Any` / unset tag on *either* side
//! is the tolerant wildcard and never mismatches. Only two concrete-but-unequal
//! schemas are a mismatch. The default posture is [loose-but-observed][Loose] —
//! a mismatch is a `tracing::warn`, not a hard error — matching the #1345 design
//! (a graph that ran yesterday must not stop running because a port was
//! re-typed). A safety-critical wiring site opts into [`Strict`][Strict] to turn
//! that warn into a typed [`Error::SchemaIdentMismatch`] at the wiring site.
//!
//! [`SchemaIdentWire`]: crate::iceoryx2::SchemaIdentWire
//! [`PortSchemaSpec`]: streamlib_processor_schema::PortSchemaSpec
//! [Loose]: SchemaValidationPosture::Loose
//! [Strict]: SchemaValidationPosture::Strict
//! [`Error::SchemaIdentMismatch`]: crate::core::error::Error::SchemaIdentMismatch

use streamlib_processor_schema::PortSchemaSpec;

use crate::core::error::{Error, Result};
use crate::iceoryx2::SchemaIdentWire;

/// How aggressively a wiring site enforces producer/consumer schema agreement.
///
/// The engine-wide default is [`Loose`](Self::Loose): a mismatch is observed
/// (warned) but the link is still wired, so a re-typed port never silently
/// stops a running graph. A safety-critical channel selects
/// [`Strict`](Self::Strict) to hard-fail the wiring instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaValidationPosture {
    /// Warn on a concrete mismatch, then wire the link anyway.
    #[default]
    Loose,
    /// Reject the wiring with [`Error::SchemaIdentMismatch`] on a concrete
    /// mismatch.
    ///
    /// [`Error::SchemaIdentMismatch`]: crate::core::error::Error::SchemaIdentMismatch
    Strict,
}

/// Whether a producer schema and a consumer schema agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaAgreement {
    /// The two ends agree — identical concrete schemas, or a wildcard
    /// (`Any` / unset) on at least one side.
    Compatible,
    /// Both ends declare a concrete schema and the two differ.
    Mismatch,
}

/// Classify two resolved port specs at a connect-time wiring site.
///
/// A wildcard ([`PortSchemaSpec::Any`]) — or an unresolved
/// [`PortSchemaSpec::Named`], which the registry never yields for a wired link
/// but is handled here defensively — on either side is
/// [`Compatible`](SchemaAgreement::Compatible): the check can only assert a
/// mismatch when *both* ends are concrete [`PortSchemaSpec::Specific`].
pub fn classify_port_schema_agreement(
    producer: &PortSchemaSpec,
    consumer: &PortSchemaSpec,
) -> SchemaAgreement {
    match (producer.specific(), consumer.specific()) {
        (Some(producer_ident), Some(consumer_ident)) => {
            if producer_ident == consumer_ident {
                SchemaAgreement::Compatible
            } else {
                SchemaAgreement::Mismatch
            }
        }
        // A wildcard / unresolved end can carry any payload — no assertion.
        _ => SchemaAgreement::Compatible,
    }
}

/// Classify a stamped inbound-frame tag against a consumer port's expected tag.
///
/// An [unset][SchemaIdentWire::is_unset] tag on either side is the wildcard and
/// never mismatches; two set-but-unequal tags are a mismatch.
pub fn classify_wire_schema_agreement(
    stamped: &SchemaIdentWire,
    expected: &SchemaIdentWire,
) -> SchemaAgreement {
    if stamped.is_unset() || expected.is_unset() {
        SchemaAgreement::Compatible
    } else if stamped == expected {
        SchemaAgreement::Compatible
    } else {
        SchemaAgreement::Mismatch
    }
}

/// Diagnostic context for a connect-time agreement check — the endpoints being
/// wired, so a warn / error names the exact link.
pub struct ConnectSchemaContext<'a> {
    pub from_processor: &'a str,
    pub from_port: &'a str,
    pub to_processor: &'a str,
    pub to_port: &'a str,
}

/// Enforce producer/consumer schema agreement at a connect-time wiring site.
///
/// Returns `Ok(())` when the two ends are [`Compatible`](SchemaAgreement::Compatible).
/// On a [`Mismatch`](SchemaAgreement::Mismatch) the [posture][SchemaValidationPosture]
/// decides: [`Loose`](SchemaValidationPosture::Loose) logs a `tracing::warn` and
/// returns `Ok(())`; [`Strict`](SchemaValidationPosture::Strict) returns
/// [`Error::SchemaIdentMismatch`] so the caller rolls the link back.
pub fn enforce_connect_schema_agreement(
    producer: &PortSchemaSpec,
    consumer: &PortSchemaSpec,
    posture: SchemaValidationPosture,
    ctx: ConnectSchemaContext<'_>,
) -> Result<()> {
    if classify_port_schema_agreement(producer, consumer) == SchemaAgreement::Compatible {
        return Ok(());
    }

    match posture {
        SchemaValidationPosture::Loose => {
            tracing::warn!(
                from_processor = ctx.from_processor,
                from_port = ctx.from_port,
                to_processor = ctx.to_processor,
                to_port = ctx.to_port,
                producer_schema = %producer,
                consumer_schema = %consumer,
                "connect: producer output schema does not match consumer input \
                 schema — wiring the link anyway (loose validation). Relax a port \
                 to `any` to silence this, or opt the channel into strict \
                 validation to hard-fail instead."
            );
            Ok(())
        }
        SchemaValidationPosture::Strict => Err(Error::SchemaIdentMismatch {
            from_processor: ctx.from_processor.to_string(),
            from_port: ctx.from_port.to_string(),
            to_processor: ctx.to_processor.to_string(),
            to_port: ctx.to_port.to_string(),
            producer_schema: producer.to_string(),
            consumer_schema: consumer.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};

    fn spec(org: &str, package: &str, ty: &str, major: u32) -> PortSchemaSpec {
        PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(package).unwrap(),
            TypeName::new(ty).unwrap(),
            SemVer::new(major, 0, 0),
        ))
    }

    fn ctx() -> ConnectSchemaContext<'static> {
        ConnectSchemaContext {
            from_processor: "producer1",
            from_port: "out",
            to_processor: "consumer1",
            to_port: "in",
        }
    }

    #[test]
    fn wildcard_on_either_side_is_compatible() {
        let a = spec("tatolab", "core", "VideoFrame", 1);
        assert_eq!(
            classify_port_schema_agreement(&PortSchemaSpec::Any, &a),
            SchemaAgreement::Compatible,
            "an `any` producer accepts any consumer",
        );
        assert_eq!(
            classify_port_schema_agreement(&a, &PortSchemaSpec::Any),
            SchemaAgreement::Compatible,
            "an `any` consumer accepts any producer",
        );
        assert_eq!(
            classify_port_schema_agreement(&PortSchemaSpec::Any, &PortSchemaSpec::Any),
            SchemaAgreement::Compatible,
        );
    }

    #[test]
    fn identical_concrete_specs_are_compatible() {
        let a = spec("tatolab", "core", "VideoFrame", 1);
        assert_eq!(
            classify_port_schema_agreement(&a, &a.clone()),
            SchemaAgreement::Compatible,
        );
    }

    /// Revert lock: two distinct concrete specs MUST classify as a mismatch.
    /// Mentally revert the comparison to always-`Compatible` and this fails —
    /// which is exactly the "no consumer reads the tag" gap #1430 closes.
    #[test]
    fn distinct_concrete_specs_mismatch() {
        let producer = spec("tatolab", "core", "VideoFrame", 1);
        let consumer = spec("tatolab", "core", "AudioFrame", 1);
        assert_eq!(
            classify_port_schema_agreement(&producer, &consumer),
            SchemaAgreement::Mismatch,
        );
        // Same type, different major version is still a concrete mismatch.
        let consumer_v2 = spec("tatolab", "core", "VideoFrame", 2);
        assert_eq!(
            classify_port_schema_agreement(&producer, &consumer_v2),
            SchemaAgreement::Mismatch,
        );
    }

    #[test]
    fn loose_mismatch_warns_but_does_not_fail() {
        let producer = spec("tatolab", "core", "VideoFrame", 1);
        let consumer = spec("tatolab", "core", "AudioFrame", 1);
        enforce_connect_schema_agreement(
            &producer,
            &consumer,
            SchemaValidationPosture::Loose,
            ctx(),
        )
        .expect("loose posture must warn, not fail, on a mismatch");
    }

    /// Revert lock for strict opt-in: a concrete mismatch under `Strict` MUST
    /// hard-fail with the typed [`Error::SchemaIdentMismatch`] naming both
    /// schemas and the link endpoints.
    #[test]
    fn strict_mismatch_hard_fails_with_typed_error() {
        let producer = spec("tatolab", "core", "VideoFrame", 1);
        let consumer = spec("tatolab", "core", "AudioFrame", 1);
        let err = enforce_connect_schema_agreement(
            &producer,
            &consumer,
            SchemaValidationPosture::Strict,
            ctx(),
        )
        .expect_err("strict posture must hard-fail on a mismatch");

        assert!(
            matches!(err, Error::SchemaIdentMismatch { .. }),
            "strict mismatch must surface Error::SchemaIdentMismatch; got {err:?}",
        );
        let msg = err.to_string();
        assert!(msg.contains("VideoFrame"), "message names producer: {msg}");
        assert!(msg.contains("AudioFrame"), "message names consumer: {msg}");
        assert!(msg.contains("producer1"), "message names endpoints: {msg}");
    }

    #[test]
    fn strict_is_silent_when_schemas_agree() {
        let a = spec("tatolab", "core", "VideoFrame", 1);
        enforce_connect_schema_agreement(&a, &a.clone(), SchemaValidationPosture::Strict, ctx())
            .expect("matching schemas must pass even under strict validation");
        enforce_connect_schema_agreement(
            &PortSchemaSpec::Any,
            &a,
            SchemaValidationPosture::Strict,
            ctx(),
        )
        .expect("a wildcard end must pass even under strict validation");
    }

    #[test]
    fn wire_agreement_treats_unset_as_wildcard() {
        let set = SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap();
        let unset = SchemaIdentWire::default();
        assert_eq!(
            classify_wire_schema_agreement(&unset, &set),
            SchemaAgreement::Compatible,
        );
        assert_eq!(
            classify_wire_schema_agreement(&set, &unset),
            SchemaAgreement::Compatible,
        );
        assert_eq!(
            classify_wire_schema_agreement(&set, &set),
            SchemaAgreement::Compatible,
        );
    }

    /// Revert lock for the runtime read path: two distinct stamped/expected
    /// tags MUST classify as a mismatch. Reverting the per-frame tag read to
    /// "ignore the tag" collapses this to `Compatible` and fails here.
    #[test]
    fn wire_agreement_flags_distinct_set_tags() {
        let stamped =
            SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap();
        let expected =
            SchemaIdentWire::from_segments("tatolab", "core", "AudioFrame", 1, 0, 0).unwrap();
        assert_eq!(
            classify_wire_schema_agreement(&stamped, &expected),
            SchemaAgreement::Mismatch,
        );
    }
}
