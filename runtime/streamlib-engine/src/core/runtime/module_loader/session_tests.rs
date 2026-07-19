// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for [`Runner::add_local`] — live registration of a `#[processor]`
//! host type under `@session/<name>@0.0.N`, with no package on disk.
//!
//! Each test uses a distinct fixture type so their `@session/<name>` ledger
//! keys never collide across the process-global registry / ledger; the tests
//! are `#[serial]` for the same reason.

use serial_test::serial;

use super::super::Runner;
use crate::core::descriptors::{Org, Package, ProcessorDescriptor, SchemaIdent, SemVer, TypeName};
use crate::core::processors::PROCESSOR_REGISTRY;

// =============================================================================
// Fixtures — `#[processor]` host types authored in-crate (no package on disk).
// Distinct names ⇒ distinct `@session/<name>` keys ⇒ no cross-test collision.
// =============================================================================

#[crate::processor("@app/local/SessionLocalAlpha", execution = manual)]
struct SessionLocalAlpha;
impl crate::core::ManualProcessor for SessionLocalAlpha::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

#[crate::processor("@app/local/SessionLocalBeta", execution = manual)]
struct SessionLocalBeta;
impl crate::core::ManualProcessor for SessionLocalBeta::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

#[crate::processor("@app/local/SessionLocalGamma", execution = manual)]
struct SessionLocalGamma;
impl crate::core::ManualProcessor for SessionLocalGamma::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// A shadow fixture: its short type name `Widget` deliberately matches an
/// installed `@tatolab/things/Widget` registered in the shadow test.
#[crate::processor("@app/local/Widget", execution = manual)]
struct SessionShadowWidget;
impl crate::core::ManualProcessor for SessionShadowWidget::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// A typed config for the config-validation fixture — a required `gain` field
/// so a malformed config JSON is genuinely rejected.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct SessionGainConfig {
    gain: f32,
}

#[crate::processor(
    "@app/local/SessionConfigured",
    execution = manual,
    config = SessionGainConfig,
)]
struct SessionConfigured;
impl crate::core::ManualProcessor for SessionConfigured::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        let _ = self.config.gain;
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

fn session_ident_for(
    module: &streamlib_idents::ModuleIdent,
    type_name: &str,
) -> Option<SchemaIdent> {
    PROCESSOR_REGISTRY.resolve_installed_processor_type(
        &module.org,
        &module.name,
        &TypeName::new(type_name).expect("valid type name"),
    )
}

// =============================================================================

/// The core acceptance path: `add_local` registers a host type with NO
/// package, manifest, dylib, or build on disk — the whole flow is the type,
/// a minted `@session/<name>@0.0.N` ident, and the registry. The registered
/// session processor is fully instantiable (descriptor + port info + a
/// non-cdylib host vtable all present).
#[test]
#[serial]
fn add_local_registers_a_session_processor_with_no_package_on_disk() {
    let runtime = Runner::new().unwrap();
    let loaded = runtime
        .add_local_blocking::<SessionLocalAlpha::Processor>(serde_json::Value::Null)
        .expect("session registration must succeed");

    // Minted under the reserved session org, at a concrete 0.0.N version.
    assert_eq!(loaded.ident.org.as_str(), "session");
    assert_eq!(loaded.ident.name.as_str(), "session-local-alpha");

    let ident = session_ident_for(&loaded.ident, "SessionLocalAlpha")
        .expect("the session processor ident must resolve after registration");
    assert_eq!(ident.org.as_str(), "session");
    assert!(PROCESSOR_REGISTRY.is_registered(&ident));
    assert!(
        PROCESSOR_REGISTRY.port_info(&ident).is_some(),
        "a registered session processor exposes port info (is instantiable)"
    );
    assert!(PROCESSOR_REGISTRY.descriptor(&ident).is_some());

    runtime
        .remove_module(loaded.ident)
        .expect("remove_module unregisters the session processor symmetrically");
    assert!(
        session_ident_for(
            &streamlib_idents::ModuleIdent::new(
                Org::new("session").unwrap(),
                Package::new("session-local-alpha").unwrap(),
                streamlib_idents::SemVerRange::Any,
            ),
            "SessionLocalAlpha",
        )
        .is_none(),
        "after remove_module the session processor is gone"
    );
}

/// A second live `add_local` of the same `@session/<name>` (not removed) is a
/// loud typed refusal — never a silent overwrite. Mentally revert the
/// ledger-collision check in `commit_session_processor_registration` and the
/// second call silently succeeds (the vtable registry dedups by ident key),
/// failing this assertion.
#[test]
#[serial]
fn add_local_refuses_a_duplicate_live_session_name() {
    let runtime = Runner::new().unwrap();
    let _first = runtime
        .add_local_blocking::<SessionLocalBeta::Processor>(serde_json::Value::Null)
        .expect("first registration succeeds");

    let err = runtime
        .add_local_blocking::<SessionLocalBeta::Processor>(serde_json::Value::Null)
        .expect_err("a duplicate live session name must be refused");
    assert!(
        matches!(err, super::errors::AddModuleError::DuplicateSessionProcessorName { .. }),
        "expected DuplicateSessionProcessorName, got {err:?}"
    );

    // Cleanup so the fixture's @session name is free for other runs.
    let ident = session_ident_for(
        &streamlib_idents::ModuleIdent::new(
            Org::new("session").unwrap(),
            Package::new("session-local-beta").unwrap(),
            streamlib_idents::SemVerRange::Any,
        ),
        "SessionLocalBeta",
    )
    .expect("registered");
    let module = streamlib_idents::ModuleIdent::new(
        ident.org.clone(),
        ident.package.clone(),
        streamlib_idents::SemVerRange::Any,
    );
    runtime.remove_module(module).expect("cleanup remove");
}

/// After `remove_module`, the same type re-registers cleanly at a FRESH
/// version — the monotonic session counter never reuses a stale ident. This
/// is the add/remove/add cycle the counter exists for.
#[test]
#[serial]
fn add_local_re_registers_after_removal_at_a_fresh_version() {
    let runtime = Runner::new().unwrap();
    let first = runtime
        .add_local_blocking::<SessionLocalGamma::Processor>(serde_json::Value::Null)
        .expect("first registration");
    let first_version = first.ident.version.clone();

    runtime
        .remove_module(first.ident)
        .expect("remove clears the ledger entry");

    let second = runtime
        .add_local_blocking::<SessionLocalGamma::Processor>(serde_json::Value::Null)
        .expect("re-registration after removal succeeds");
    assert_ne!(
        second.ident.version, first_version,
        "the re-registered version must be fresh, not the stale prior version"
    );

    runtime.remove_module(second.ident).expect("cleanup remove");
}

/// A session processor whose short type name shadows an already-registered
/// installed processor keeps BOTH addressable by full ident — never
/// overwrites. The registry keys on the structured `SchemaIdent`, so
/// `@tatolab/things/Widget` and `@session/widget/Widget` coexist.
#[test]
#[serial]
fn add_local_shadow_keeps_both_installed_and_session_addressable() {
    let runtime = Runner::new().unwrap();

    let installed = SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("things").unwrap(),
        TypeName::new("Widget").unwrap(),
        SemVer::new(1, 0, 0),
    );
    PROCESSOR_REGISTRY
        .register_descriptor_only(ProcessorDescriptor::new(installed.clone(), "installed widget"))
        .expect("installed Widget registers");

    let loaded = runtime
        .add_local_blocking::<SessionShadowWidget::Processor>(serde_json::Value::Null)
        .expect("session Widget registers despite the short-name shadow");

    let session_widget = session_ident_for(&loaded.ident, "Widget").expect("session Widget resolves");
    assert_ne!(session_widget, installed);
    assert!(
        PROCESSOR_REGISTRY.descriptor(&installed).is_some(),
        "the installed Widget stays addressable"
    );
    assert!(
        PROCESSOR_REGISTRY.is_registered(&session_widget),
        "the session Widget is addressable"
    );

    // The shadow helper reports the cross-package collision (used to warn).
    let shadowed = PROCESSOR_REGISTRY.warn_on_short_name_shadow(&session_widget);
    assert!(
        shadowed.contains(&installed),
        "the installed Widget is reported as shadowed by the session Widget"
    );

    runtime.remove_module(loaded.ident).expect("cleanup remove");
}

/// A malformed config is refused BEFORE registering — a session type never
/// registers with a config its own `Config` type rejects.
#[test]
#[serial]
fn add_local_rejects_a_config_that_does_not_match_the_type() {
    let runtime = Runner::new().unwrap();
    let err = runtime
        .add_local_blocking::<SessionConfigured::Processor>(serde_json::json!({ "gain": "not-a-number" }))
        .expect_err("a config that doesn't fit P::Config must be refused");
    assert!(
        matches!(err, super::errors::AddModuleError::SessionProcessorConfigInvalid { .. }),
        "expected SessionProcessorConfigInvalid, got {err:?}"
    );

    // The type never registered — nothing to clean up.
    assert!(
        session_ident_for(
            &streamlib_idents::ModuleIdent::new(
                Org::new("session").unwrap(),
                Package::new("session-configured").unwrap(),
                streamlib_idents::SemVerRange::Any,
            ),
            "SessionConfigured",
        )
        .is_none(),
        "a refused registration leaves zero registry residue"
    );
}

/// A well-formed config validates and registers; the type is then live.
#[test]
#[serial]
fn add_local_accepts_a_well_formed_config() {
    let runtime = Runner::new().unwrap();
    let loaded = runtime
        .add_local_blocking::<SessionConfigured::Processor>(serde_json::json!({ "gain": 1.5 }))
        .expect("a valid config registers");
    let ident = session_ident_for(&loaded.ident, "SessionConfigured").expect("registered");
    assert!(PROCESSOR_REGISTRY.is_registered(&ident));
    runtime.remove_module(loaded.ident).expect("cleanup remove");
}
