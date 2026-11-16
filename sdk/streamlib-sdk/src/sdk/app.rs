// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! [`App`] — thin authoring sugar over a [`Runner`](crate::sdk::runtime::Runner)
//! and the one stream it loads.
//!
//! Every method is a direct delegation to an existing engine or stream op;
//! `App` adds no runtime state of its own and owns no graph capability the
//! runtime doesn't. [`App::runner`] and [`App::stream`] are the escape hatches
//! for anything the sugar doesn't cover.

use std::sync::Arc;

use serde::Serialize;

use crate::sdk::descriptors::NodeClassImportPath;
use crate::sdk::error::{Error, Result};
use crate::sdk::graph::{InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef};
use crate::sdk::nodes::{Config, GeneratedNode, NodeSpec};
use crate::sdk::runtime::{
    LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner, StreamLocation,
};

pub type AppPortEndpoint<'a> = (&'a AddedNode, &'a str);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AddedNode {
    node_run_id: NodeRunId,
    display_name: String,
}

impl AddedNode {
    pub fn node_run_id(&self) -> &NodeRunId {
        &self.node_run_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

/// The stream id of the one stream an [`App`] loads; its display name too.
pub const THE_STREAM_ID_AN_APP_LOADS: &str = "main";

pub struct App {
    runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
}

impl App {
    /// Build an `App` over a fresh [`Runner`] holding one empty stream, the stream of
    /// `stream_location`, under the id [`THE_STREAM_ID_AN_APP_LOADS`].
    pub fn new_at_stream_location(stream_location: StreamLocation) -> Result<Self> {
        let runner = Runner::new()?;
        let stream =
            runner.load_an_empty_stream(OptionsForLoadingOneStream::at_stream_location(
                THE_STREAM_ID_AN_APP_LOADS,
                stream_location,
            ))?;
        Ok(Self { runner, stream })
    }

    pub fn add(
        &self,
        node_class_import_path: NodeClassImportPath,
        config: impl Serialize,
        display_name: Option<&str>,
    ) -> Result<AddedNode> {
        let config = to_config_value(config)?;
        self.add_spec(NodeSpec::new(node_class_import_path, config), display_name)
    }

    pub fn add_local<P>(
        &self,
        config: impl Serialize,
        display_name: Option<&str>,
    ) -> Result<AddedNode>
    where
        P: GeneratedNode + 'static,
        P::Config: Config,
    {
        let config = to_config_value(config)?;
        let node_class_import_path = self.stream.add_local::<P>(config.clone())?;
        self.add_spec(NodeSpec::new(node_class_import_path, config), display_name)
    }

    pub fn connect(
        &self,
        from: AppPortEndpoint<'_>,
        to: AppPortEndpoint<'_>,
    ) -> Result<LinkUniqueId> {
        self.stream.connect(
            OutputLinkPortRef::new(from.0.node_run_id(), from.1),
            InputLinkPortRef::new(to.0.node_run_id(), to.1),
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
        mut spec: NodeSpec,
        requested_display_name: Option<&str>,
    ) -> Result<AddedNode> {
        spec.display_name = requested_display_name.map(str::to_string);
        let added = self.stream.add_node_reporting_its_name(spec)?;
        Ok(AddedNode {
            node_run_id: added.node_run_id,
            display_name: added.name,
        })
    }
}

/// Encode a caller config value to the JSON the runtime carries, mapping a
/// serialization failure to [`Error::Configuration`].
fn to_config_value(config: impl Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(config)
        .map_err(|e| Error::Configuration(format!("node config is not serializable: {e}")))
}
