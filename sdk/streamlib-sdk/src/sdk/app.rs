// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`App`] — thin authoring sugar over a [`Runner`](crate::sdk::runtime::Runner)
//! and the one stream it loads.
//!
//! Every method is a direct delegation to an existing engine or stream op;
//! `App` adds no runtime state of its own and owns no graph capability the
//! runtime doesn't. [`App::runner`] and [`App::stream`] are the escape hatches
//! for anything the sugar doesn't cover.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::sdk::descriptors::ProcessorClassImportPath;
use crate::sdk::error::{Error, Result};
use crate::sdk::graph::{InputLinkPortRef, LinkUniqueId, OutputLinkPortRef, ProcessorUniqueId};
use crate::sdk::processors::{Config, GeneratedProcessor, ProcessorSpec};
use crate::sdk::runtime::{LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner};

/// A `(processor, port)` endpoint for [`App::connect`]. The processor is
/// referenced by the [`AddedProcessor`] an `add`/`add_local` call returned;
/// the port is the source-declared port name.
pub type AppPortEndpoint<'a> = (&'a AddedProcessor, &'a str);

/// A processor [`App::add`] or [`App::add_local`] put in the graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AddedProcessor {
    processor_id: ProcessorUniqueId,
    display_name: String,
}

impl AddedProcessor {
    /// The engine's id for this processor — what `tatolab graph` shows.
    pub fn processor_id(&self) -> &ProcessorUniqueId {
        &self.processor_id
    }

    /// The display name the graph assigned this processor.
    ///
    /// The requested one only when nothing else in the graph already answered
    /// to it: the graph appends a counter to a duplicate, so a caller that
    /// asked for a name cannot assume it got that name.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

/// The name of the one stream an [`App`] loads.
pub const THE_STREAM_NAME_AN_APP_LOADS: &str = "main";

/// Thin authoring sugar over a [`Runner`] and one stream loaded in it:
/// construct, add processors, connect ports, run.
///
/// `App` is not a parallel runtime — it holds one `Runner` and the stream it
/// loaded, and forwards to them. Errors surface as the underlying engine
/// [`Error`] variants unchanged. For any capability the sugar omits, drop to
/// [`App::runner`] or [`App::stream`].
pub struct App {
    runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
}

impl App {
    /// Build an `App` over a fresh [`Runner`] holding one empty stream named
    /// [`THE_STREAM_NAME_AN_APP_LOADS`] whose project directory is the current
    /// directory.
    pub fn new() -> Result<Self> {
        let project_directory = std::env::current_dir().map_err(|e| {
            Error::Configuration(format!(
                "an App's stream lives in the current directory, which cannot be read: {e}"
            ))
        })?;
        Self::new_in_project_directory(project_directory)
    }

    /// Build an `App` over a fresh [`Runner`] holding one empty stream named
    /// [`THE_STREAM_NAME_AN_APP_LOADS`] whose project directory is
    /// `project_directory`.
    pub fn new_in_project_directory(project_directory: impl Into<PathBuf>) -> Result<Self> {
        let runner = Runner::new()?;
        let stream = runner.load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(project_directory)
                .named(THE_STREAM_NAME_AN_APP_LOADS),
        )?;
        Ok(Self { runner, stream })
    }

    /// Add a processor by the import path of its class, configured from
    /// `config`. `config` is any [`Serialize`] value (a generated config `Bag`,
    /// a plain struct, or a [`serde_json::Value`]); it is encoded to JSON and
    /// handed to the runtime unchanged. A `display_name` of `None` takes the
    /// class's short name.
    pub fn add(
        &self,
        processor_class_import_path: ProcessorClassImportPath,
        config: impl Serialize,
        display_name: Option<&str>,
    ) -> Result<AddedProcessor> {
        let config = to_config_value(config)?;
        self.add_spec(
            ProcessorSpec::new(processor_class_import_path, config),
            display_name,
        )
    }

    /// Register a `#[processor]`-annotated host type `P` on the processor
    /// registry (no package on disk) and immediately instantiate it,
    /// returning the connectable [`AddedProcessor`]. `config` is validated
    /// against `P::Config` at registration and used as the instance config.
    pub fn add_local<P>(
        &self,
        config: impl Serialize,
        display_name: Option<&str>,
    ) -> Result<AddedProcessor>
    where
        P: GeneratedProcessor + 'static,
        P::Config: Config,
    {
        let config = to_config_value(config)?;
        let processor_class_import_path = self.stream.add_local::<P>(config.clone())?;
        self.add_spec(
            ProcessorSpec::new(processor_class_import_path, config),
            display_name,
        )
    }

    /// Connect an output endpoint to an input endpoint — `((&from, "out"),
    /// (&to, "in"))`. A nonexistent port surfaces the runtime's
    /// [`Error::ProcessorPortNotFound`] unchanged.
    pub fn connect(
        &self,
        from: AppPortEndpoint<'_>,
        to: AppPortEndpoint<'_>,
    ) -> Result<LinkUniqueId> {
        self.stream.connect(
            OutputLinkPortRef::new(from.0.processor_id(), from.1),
            InputLinkPortRef::new(to.0.processor_id(), to.1),
        )
    }

    /// Own the machine's shutdown signals, start the stream, block until it
    /// ends, then shut the engine down.
    pub fn run(&self) -> Result<()> {
        let run_outcome = self.runner.run_owning_the_machine_shutdown_signals(|| {
            self.stream.start()?;
            self.runner.wait_until_the_stream_ends(&self.stream)
        });
        let shut_down_outcome = self.runner.shut_down();
        run_outcome.and(shut_down_outcome)
    }

    /// The underlying [`Runner`] — the escape
    /// hatch for anything the sugar doesn't wrap.
    pub fn runner(&self) -> &Arc<Runner> {
        &self.runner
    }

    /// The stream this `App` loaded — the escape hatch for any graph
    /// operation the sugar doesn't wrap.
    pub fn stream(&self) -> &Arc<LoadedStreamInThisRuntime> {
        &self.stream
    }

    /// Add `spec` under an optional requested display name.
    fn add_spec(
        &self,
        mut spec: ProcessorSpec,
        requested_display_name: Option<&str>,
    ) -> Result<AddedProcessor> {
        spec.display_name = requested_display_name.map(str::to_string);
        let added = self.stream.add_processor_reporting_its_name(spec)?;
        Ok(AddedProcessor {
            processor_id: added.processor_id,
            display_name: added.name,
        })
    }
}

/// Encode a caller config value to the JSON the runtime carries, mapping a
/// serialization failure to [`Error::Configuration`].
fn to_config_value(config: impl Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(config)
        .map_err(|e| Error::Configuration(format!("processor config is not serializable: {e}")))
}
