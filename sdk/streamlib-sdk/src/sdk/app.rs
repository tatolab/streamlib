// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`App`] — thin authoring sugar over [`Runner`](crate::sdk::runtime::Runner).
//!
//! Every method is a direct delegation to an existing `Runner` op; `App`
//! adds no runtime state of its own and owns no graph capability the runtime
//! doesn't. [`App::runner`] is the escape hatch back to the full `Runner`
//! surface for anything the sugar doesn't cover.

use std::sync::Arc;

use serde::Serialize;

use crate::sdk::RunnerAutoBuild;
use crate::sdk::error::{Error, Result};
use crate::sdk::graph::{InputLinkPortRef, LinkUniqueId, OutputLinkPortRef, ProcessorUniqueId};
use crate::sdk::processors::{Config, GeneratedProcessor, ProcessorSpec, ProcessorTypeReference};
use crate::sdk::runtime::{ConnectOptions, Runner};

/// A `(processor, port)` endpoint for [`App::connect`]. The processor is
/// referenced by the [`ProcessorUniqueId`] an `add`/`add_local` call returned;
/// the port is the source-declared port name.
pub type AppPortEndpoint<'a> = (&'a ProcessorUniqueId, &'a str);

/// Thin authoring sugar over [`Runner`]: construct,
/// add processors, connect ports, run.
///
/// `App` is not a parallel runtime — it holds one `Runner` and forwards to it.
/// Errors surface as the underlying engine [`Error`] variants unchanged. For
/// any capability the sugar omits, drop to [`App::runner`].
pub struct App {
    runner: Arc<Runner>,
}

impl App {
    /// Build an `App` over a `Runner` with the default polyglot build
    /// orchestrator wired (the [`RunnerAutoBuild::with_auto_build`] path), so a
    /// version-free [`add`](Self::add) reference to a not-yet-built package
    /// materializes from source on demand.
    pub fn new() -> Result<Self> {
        Ok(Self {
            runner: Runner::with_auto_build()?,
        })
    }

    /// Add a processor by type reference, configured from `config`. `config` is
    /// any [`Serialize`] value (a generated config `Bag`, a plain struct, or a
    /// [`serde_json::Value`]); it is encoded to JSON and handed to the runtime
    /// unchanged.
    pub fn add(
        &self,
        processor_ref: impl Into<ProcessorTypeReference>,
        config: impl Serialize,
    ) -> Result<ProcessorUniqueId> {
        let config = to_config_value(config)?;
        self.runner
            .add_processor(ProcessorSpec::new(processor_ref, config))
    }

    /// Register a `#[processor]`-annotated host type `P` live (no package on
    /// disk — the [`Runner::add_local_blocking`](crate::sdk::runtime::Runner)
    /// path) and immediately instantiate it, returning the connectable
    /// [`ProcessorUniqueId`]. `config` is validated against `P::Config` at
    /// registration and used as the instance config.
    pub fn add_local<P>(&self, config: impl Serialize) -> Result<ProcessorUniqueId>
    where
        P: GeneratedProcessor + 'static,
        P::Config: Config,
    {
        let config = to_config_value(config)?;
        let loaded = self.runner.add_local_blocking::<P>(config.clone())?;
        // The minted module is `@session/<name>@0.0.N`; its one processor's
        // short type name comes from the type's own descriptor. Resolve it as
        // an installed `(org, package, type)` triple — the session processor is
        // already registered, so this hits the fast path with no disk load.
        let descriptor = P::descriptor().ok_or_else(|| {
            Error::Configuration(format!(
                "add_local::<{}>() registered a session module but the type exposes no descriptor",
                std::any::type_name::<P>()
            ))
        })?;
        let reference = ProcessorTypeReference::ResolveToInstalled {
            org: loaded.ident.org,
            package: loaded.ident.name,
            r#type: descriptor.name.r#type,
        };
        self.runner
            .add_processor(ProcessorSpec::new(reference, config))
    }

    /// Connect an output endpoint to an input endpoint — `((&from, "out"),
    /// (&to, "in"))`. A nonexistent port surfaces the runtime's
    /// [`Error::ProcessorPortNotFound`] unchanged.
    pub fn connect(
        &self,
        from: AppPortEndpoint<'_>,
        to: AppPortEndpoint<'_>,
    ) -> Result<LinkUniqueId> {
        self.runner.connect(
            OutputLinkPortRef::new(from.0, from.1),
            InputLinkPortRef::new(to.0, to.1),
        )
    }

    /// Connect two endpoints under explicit [`ConnectOptions`] — the strict
    /// schema-validation opt-in for a safety-critical channel. Under
    /// [`ConnectOptions::strict`] a concrete producer/consumer schema mismatch
    /// surfaces the runtime's [`Error::SchemaIdentMismatch`] at the wiring site
    /// instead of only warning.
    pub fn connect_with(
        &self,
        from: AppPortEndpoint<'_>,
        to: AppPortEndpoint<'_>,
        options: ConnectOptions,
    ) -> Result<LinkUniqueId> {
        self.runner.connect_with(
            OutputLinkPortRef::new(from.0, from.1),
            InputLinkPortRef::new(to.0, to.1),
            options,
        )
    }

    /// Start the graph, then block until a shutdown signal
    /// ([`Runner::start`](crate::sdk::runtime::Runner) +
    /// [`wait_for_signal`](crate::sdk::runtime::Runner)).
    pub fn run(&self) -> Result<()> {
        self.runner.start()?;
        self.runner.wait_for_signal()
    }

    /// The underlying [`Runner`] — the escape
    /// hatch for anything the sugar doesn't wrap.
    pub fn runner(&self) -> &Arc<Runner> {
        &self.runner
    }
}

/// Encode a caller config value to the JSON the runtime carries, mapping a
/// serialization failure to [`Error::Configuration`].
fn to_config_value(config: impl Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(config)
        .map_err(|e| Error::Configuration(format!("processor config is not serializable: {e}")))
}
