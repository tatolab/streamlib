// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`Runner::add_local`] — register a `#[processor]`-annotated host type
//! live at runtime, with no package on disk.
//!
//! In-app authoring is register-on-live-runtime: an already-compiled host
//! type is minted a fresh `@session/<name>@0.0.N` identity (distinct from
//! its compile-time `@app/local/<Type>` identity) and registered onto the
//! one [`PROCESSOR_REGISTRY`] through the same
//! [`ModuleLoadRegistrationStaging`] → commit → [`ledger`] seam a disk-backed
//! module load uses — so [`Runner::remove_module`] unregisters it
//! symmetrically. No manifest, no dylib, no build.
//!
//! [`PROCESSOR_REGISTRY`]: crate::core::processors::PROCESSOR_REGISTRY
//! [`ModuleLoadRegistrationStaging`]: super::staging::ModuleLoadRegistrationStaging
//! [`ledger`]: super::ledger
//! [`Runner::remove_module`]: super::super::Runner::remove_module

use std::time::Instant;

use tokio::sync::broadcast;

use streamlib_plugin_abi::ProcessorVTable;

use super::super::Runner;
use super::super::runtime::TokioRuntimeVariant;
use super::added_module::MODULE_EVENT_CHANNEL_CAPACITY;
use super::errors::AddModuleError;
use super::{AddedModule, LoadedModule, ModuleLoadEvent};
use crate::core::descriptors::{ProcessorDescriptor, SchemaIdent};
use crate::core::processors::{Config, GeneratedProcessor};

/// The prepared (or refused-before-spawn) outcome of an [`Runner::add_local`]
/// call. The module ident is derived even on the failure paths so the
/// returned [`AddedModule`] handle always names a `@session/…` module.
struct PreparedSessionRegistration {
    module: streamlib_idents::ModuleIdent,
    version: streamlib_idents::SemVer,
    /// The re-homed descriptor + monomorphized host vtable, or the typed
    /// refusal to surface through the load future.
    outcome: std::result::Result<(ProcessorDescriptor, &'static ProcessorVTable), AddModuleError>,
}

impl Runner {
    /// Register a `#[processor]`-annotated host type live at runtime, minting
    /// it a fresh `@session/<name>@0.0.N` identity on the one
    /// [`PROCESSOR_REGISTRY`]. In-app authoring with no package on disk: no
    /// manifest, no dylib, no build. Returns an [`AddedModule`] (a `Future`
    /// whose work is already running); a session registration resolves almost
    /// immediately.
    ///
    /// `config` is validated against the type's `Config` before registering —
    /// a config that does not deserialize into `P::Config` is refused with
    /// [`AddModuleError::SessionProcessorConfigInvalid`], so a session type
    /// never registers with a config its own schema rejects.
    /// [`serde_json::Value::Null`] means "use the type's default config" and
    /// always validates.
    ///
    /// A short-type-name collision with an already-registered (installed or
    /// session) processor **warns** and keeps both addressable by full ident;
    /// it never overwrites. A live same-`@session/<name>` registration is
    /// refused with [`AddModuleError::DuplicateSessionProcessorName`] — remove
    /// it first ([`Runner::remove_module`]).
    ///
    /// [`PROCESSOR_REGISTRY`]: crate::core::processors::PROCESSOR_REGISTRY
    /// [`Runner::remove_module`]: Self::remove_module
    #[must_use = "the returned AddedModule cancels on drop — await it or pass it to await_modules"]
    #[tracing::instrument(skip_all, fields(processor = std::any::type_name::<P>()))]
    pub fn add_local<P>(&self, config: serde_json::Value) -> AddedModule
    where
        P: GeneratedProcessor + 'static,
        P::Config: Config,
    {
        let prepared = prepare_session_registration::<P>(config);
        self.spawn_session_registration(prepared)
    }

    /// Synchronous convenience for [`Self::add_local`]: drive the session
    /// registration to completion and return the [`LoadedModule`] (whose
    /// `ident` is the minted `@session/<name>@0.0.N`). For simple `fn main`
    /// examples and tests. Returns [`AddModuleError::BlockingCallFromAsyncContext`]
    /// (never panics) when called from inside a tokio runtime — use the async
    /// surface there.
    pub fn add_local_blocking<P>(
        &self,
        config: serde_json::Value,
    ) -> std::result::Result<LoadedModule, AddModuleError>
    where
        P: GeneratedProcessor + 'static,
        P::Config: Config,
    {
        if matches!(
            self.tokio_runtime_variant,
            TokioRuntimeVariant::ExternalTokioHandle(_)
        ) {
            let prepared = prepare_session_registration::<P>(config);
            return Err(AddModuleError::BlockingCallFromAsyncContext {
                module: prepared.module,
            });
        }
        let added = self.add_local::<P>(config);
        match &self.tokio_runtime_variant {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => rt.block_on(added),
            // Guarded above — the external arm returned already.
            TokioRuntimeVariant::ExternalTokioHandle(_) => unreachable!(),
        }
    }

    /// Spawn the prepared session registration onto the runtime's tokio
    /// handle and wrap it as an [`AddedModule`], mirroring
    /// [`Runner::add_module_with`]'s eager-load shape.
    fn spawn_session_registration(&self, prepared: PreparedSessionRegistration) -> AddedModule {
        let module = prepared.module.clone();
        let version = prepared.version;
        let outcome = prepared.outcome;

        let (tx, initial_rx) = broadcast::channel(MODULE_EVENT_CHANNEL_CAPACITY);
        let events = tx.clone();
        let module_for_task = module.clone();

        let join = self
            .tokio_runtime_variant
            .handle()
            .spawn_blocking(move || {
                run_session_registration(module_for_task, version, outcome, &events)
            });

        AddedModule::new(module, join, tx, initial_rx)
    }
}

/// Derive the `@session/<name>@0.0.N` identity for `P`, validate `config`
/// against `P::Config`, and monomorphize its host vtable — capturing any
/// refusal to surface through the load future. Fallible steps that can't
/// bind a real module ident fall back to `@session/local@0.0.N` so the
/// returned handle still names a session module.
fn prepare_session_registration<P>(config: serde_json::Value) -> PreparedSessionRegistration
where
    P: GeneratedProcessor + 'static,
    P::Config: Config,
{
    let type_name = std::any::type_name::<P>().to_string();

    let Some(descriptor) = <P as GeneratedProcessor>::descriptor() else {
        let fallback = fallback_session_module();
        return PreparedSessionRegistration {
            module: fallback.module,
            version: fallback.version,
            outcome: Err(AddModuleError::SessionProcessorHasNoDescriptor { type_name }),
        };
    };

    let processor_type_name = descriptor.name.r#type.clone();
    let session_name = kebab_case(processor_type_name.as_str());

    let minted = match streamlib_idents::mint_session_module_ident(&session_name) {
        Ok(minted) => minted,
        Err(e) => {
            let fallback = fallback_session_module();
            return PreparedSessionRegistration {
                module: fallback.module,
                version: fallback.version,
                outcome: Err(AddModuleError::SessionProcessorNameInvalid {
                    type_name,
                    detail: e.to_string(),
                }),
            };
        }
    };

    if !config.is_null() {
        if let Err(e) = serde_json::from_value::<P::Config>(config) {
            return PreparedSessionRegistration {
                module: minted.module,
                version: minted.version,
                outcome: Err(AddModuleError::SessionProcessorConfigInvalid {
                    type_name,
                    detail: e.to_string(),
                }),
            };
        }
    }

    let mut descriptor = descriptor;
    descriptor.name = SchemaIdent::new(
        minted.module.org.clone(),
        minted.package.clone(),
        processor_type_name,
        minted.version,
    );
    let vtable = crate::core::plugin::processor_vtable::vtable_for::<P>();

    PreparedSessionRegistration {
        module: minted.module,
        version: minted.version,
        outcome: Ok((descriptor, vtable)),
    }
}

/// Run a prepared session registration on the spawned task: on the `Ok`
/// arm, stage + commit the one processor through the shared staging seam;
/// emit the terminal load event either way.
fn run_session_registration(
    module: streamlib_idents::ModuleIdent,
    version: streamlib_idents::SemVer,
    outcome: std::result::Result<(ProcessorDescriptor, &'static ProcessorVTable), AddModuleError>,
    events: &broadcast::Sender<ModuleLoadEvent>,
) -> std::result::Result<LoadedModule, AddModuleError> {
    let start = Instant::now();
    let _ = events.send(ModuleLoadEvent::Started {
        ident: module.clone(),
    });

    let result = outcome.and_then(|(descriptor, vtable)| {
        let staging = super::staging::ModuleLoadRegistrationStaging::new();
        super::staging::commit_session_processor_registration(
            &staging, &module, version, descriptor, vtable,
        )
        .map(|_ident| ())
    });

    match result {
        Ok(()) => {
            let _ = events.send(ModuleLoadEvent::Completed {
                ident: module.clone(),
                took: start.elapsed(),
            });
            Ok(LoadedModule { ident: module })
        }
        Err(e) => {
            let _ = events.send(ModuleLoadEvent::Failed {
                ident: module.clone(),
                error: e.to_string(),
            });
            Err(e)
        }
    }
}

/// A `@session/local@0.0.N` fallback identity for the pre-registration
/// failure paths (no descriptor / un-mintable name) so the returned
/// [`AddedModule`] handle still names a session module.
fn fallback_session_module() -> streamlib_idents::MintedSessionIdent {
    streamlib_idents::mint_session_module_ident("local")
        .expect("`local` is a valid session package name")
}

/// Kebab-case a PascalCase processor type name into a `@session/<name>`
/// package segment: `TestMockProcessor` → `test-mock-processor`. A `-` is
/// inserted before each interior uppercase run boundary; underscores map to
/// `-`; the result is lowercased and de-duplicated / trimmed of `-`.
fn kebab_case(type_name: &str) -> String {
    let mut out = String::with_capacity(type_name.len() + 4);
    let mut prev_was_lower_or_digit = false;
    for ch in type_name.chars() {
        if ch == '_' || ch == '-' {
            if !out.ends_with('-') {
                out.push('-');
            }
            prev_was_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() {
            if prev_was_lower_or_digit && !out.ends_with('-') {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            prev_was_lower_or_digit = false;
        } else {
            out.push(ch);
            prev_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod kebab_tests {
    use super::kebab_case;

    #[test]
    fn kebabs_pascal_case_type_names() {
        assert_eq!(kebab_case("TestMockProcessor"), "test-mock-processor");
        assert_eq!(kebab_case("Camera"), "camera");
        assert_eq!(kebab_case("Widget"), "widget");
        assert_eq!(kebab_case("HttpV2Sink"), "http-v2-sink");
        assert_eq!(kebab_case("Already_snake"), "already-snake");
    }
}
