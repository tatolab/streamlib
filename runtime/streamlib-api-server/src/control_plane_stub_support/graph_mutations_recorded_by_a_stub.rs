// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The graph mutation ops of a stub runtime that records each call it is sent.

/// What a stub runtime answers an `add_node` with when the test has armed a
/// refusal, standing in for an engine-side one — a name already taken, an
/// unknown class: the engine error each refused add is made of.
pub(crate) type ArmedAddNodeRefusal = ::std::sync::Arc<
    ::parking_lot::Mutex<
        Option<
            ::std::sync::Arc<
                dyn Fn() -> ::streamlib::sdk::error::Error
                    + ::std::marker::Send
                    + ::std::marker::Sync,
            >,
        >,
    >,
>;

macro_rules! graph_mutation_ops_record_the_call {
    () => {
        fn add_node_async(
            &self,
            _spec: ::streamlib::sdk::nodes::NodeSpec,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::NodeInTheGraph>,
        > {
            unreachable!("the MCP front end edits a graph through `apply_a_stream_graph_operation_live` alone")
        }
        fn the_node_named(
            &self,
            _node_name: &str,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::NodeInTheGraph> {
            unreachable!("the MCP front end names a node by its UUID, never looks one up by name")
        }
        fn remove_node_async(
            &self,
            _node_run_id: ::streamlib::sdk::graph::NodeRunId,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            unreachable!("the MCP front end edits a graph through `apply_a_stream_graph_operation_live` alone")
        }
        fn connect_async(
            &self,
            _from: ::streamlib::sdk::graph::OutputLinkPortRef,
            _to: ::streamlib::sdk::graph::InputLinkPortRef,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkUniqueId>,
        > {
            unreachable!("the MCP front end edits a graph through `apply_a_stream_graph_operation_live` alone")
        }
        fn disconnect_async(
            &self,
            _link_id: ::streamlib::sdk::graph::LinkUniqueId,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            unreachable!("the MCP front end edits a graph through `apply_a_stream_graph_operation_live` alone")
        }
        fn reconcile_each_stream_fragment_to_the_stream_it_references_live(
            &self,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn what_a_live_connect_makes_of_the_link(
            &self,
            _link: &::streamlib_project_repository::StreamGraphDocumentLink,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::WhatALiveConnectMakesOfALink> {
            Ok(::streamlib::sdk::runtime::WhatALiveConnectMakesOfALink::Makes(None))
        }
        fn what_a_live_connect_makes_of_links_with_node_types_ports(
            &self,
            _held: &::streamlib_project_repository::StreamGraphDocumentLinkEnd,
            _held_direction: ::streamlib::sdk::error::PortDirection,
            node_types_ports: &[::streamlib::sdk::runtime::ANodeTypesPort<'_>],
        ) -> ::streamlib::sdk::error::Result<::std::vec::Vec<::streamlib::sdk::error::Result<::streamlib::sdk::runtime::WhatALiveConnectMakesOfALink>>> {
            Ok(node_types_ports.iter().map(|_| Ok(::streamlib::sdk::runtime::WhatALiveConnectMakesOfALink::Makes(None))).collect())
        }
        fn the_name_a_node_added_with_none_takes(
            &self,
            node_type: &str,
            _names_at_the_level: &::std::collections::HashSet<String>,
        ) -> ::streamlib::sdk::error::Result<String> {
            Ok(node_type.rsplit(':').next().unwrap_or(node_type).to_lowercase())
        }
        fn refuse_a_node_a_live_add_would_refuse(
            &self,
            _node_type: &str,
            _node_name: &str,
            _input_values: &::serde_json::Map<String, ::serde_json::Value>,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn the_ports_a_node_type_declares(
            &self,
            _node_type: &str,
        ) -> ::std::option::Option<::streamlib::sdk::graph::GraphNodePorts> {
            None
        }
        fn the_config_a_node_holding_the_input_values_takes(
            &self,
            _node_type: &str,
            input_values: &::serde_json::Map<String, ::serde_json::Value>,
        ) -> ::streamlib::sdk::error::Result<::serde_json::Value> {
            Ok(::serde_json::Value::Object(input_values.clone()))
        }
        fn refuse_an_input_value_its_input_does_not_take(
            &self,
            _node_type: &::streamlib::sdk::descriptors::NodeClassImportPath,
            _node_name: &str,
            _input_values_after_the_set: &::serde_json::Map<String, ::serde_json::Value>,
            _input: &str,
            _value: ::std::option::Option<&::serde_json::Value>,
        ) -> ::std::result::Result<(), ::streamlib::sdk::graph::input_values::InputValueRefusal> {
            Ok(())
        }
        fn refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes(
            &self,
            _node_type: &::streamlib::sdk::descriptors::NodeClassImportPath,
            _node_name: &str,
            _input: &str,
            _value: ::std::option::Option<&::serde_json::Value>,
        ) -> ::std::result::Result<(), ::streamlib::sdk::graph::input_values::InputValueRefusal> {
            Ok(())
        }
        fn set_input_values_live(
            &self,
            _sets: &[::streamlib::sdk::graph::input_values::InputValueToSetLive],
        ) -> ::std::result::Result<
            ::streamlib::sdk::graph::input_values::InputValuesSetLive,
            ::streamlib::sdk::graph::input_values::LiveInputValueSetRefusal,
        > {
            Ok(::streamlib::sdk::graph::input_values::InputValuesSetLive::OnTheGraph)
        }
        fn node_type_takes_a_config(
            &self,
            _node_type: &::streamlib::sdk::descriptors::NodeClassImportPath,
        ) -> bool {
            true
        }
        fn an_exposure_of_config_fields_is_recorded_on(
            &self,
            _node_type: &::streamlib::sdk::descriptors::NodeClassImportPath,
            _exposed_config_fields: &[String],
        ) -> ::streamlib::sdk::error::Result<bool> {
            Ok(true)
        }
        fn apply_a_stream_graph_operation_live(
            &self,
            graph_operation: ::streamlib_project_repository::StreamGraphOperation,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib_project_repository::StreamGraphOperation>,
        > {
            let mut applied = graph_operation.clone();
            let mut name_refused = None;
            if let ::streamlib_project_repository::StreamGraphOperation::AddNode { node, .. } =
                &mut applied
            {
                if node.name.is_empty() {
                    node.name = $crate::control_plane_stub_support::STUB_ADDED_NODE_NAME.to_string();
                } else {
                    match ::streamlib::sdk::graph::cast_exposed_name_to_url_safe(&node.name) {
                        Ok(cast) => node.name = cast.into_owned(),
                        Err(refusal) => name_refused = Some(refusal),
                    }
                }
            }
            let is_an_add = matches!(
                applied,
                ::streamlib_project_repository::StreamGraphOperation::AddNode { .. }
            );
            self.recorded_graph_mutations.lock().push(graph_operation);
            let armed_refusal = is_an_add
                .then(|| self.armed_add_node_refusal.lock().clone())
                .flatten();
            let answers_after = is_an_add.then_some(self.add_node_answers_after).flatten();
            Box::pin(async move {
                if let Some(answers_after) = answers_after {
                    ::tokio::time::sleep(answers_after).await;
                }
                if let Some(refusal) = name_refused {
                    return Err(refusal.into());
                }
                match armed_refusal {
                    Some(the_refusal) => Err(the_refusal()),
                    None => Ok(applied),
                }
            })
        }
        fn add_node(
            &self,
            _spec: ::streamlib::sdk::nodes::NodeSpec,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::NodeRunId> {
            unreachable!("the MCP front end awaits the async op, never the blocking wrapper")
        }
        fn remove_node(
            &self,
            _node_run_id: &::streamlib::sdk::graph::NodeRunId,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!("the MCP front end awaits the async op, never the blocking wrapper")
        }
        fn connect(
            &self,
            _from: ::streamlib::sdk::graph::OutputLinkPortRef,
            _to: ::streamlib::sdk::graph::InputLinkPortRef,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkUniqueId> {
            unreachable!("the MCP front end awaits the async op, never the blocking wrapper")
        }
        fn disconnect(
            &self,
            _link_id: &::streamlib::sdk::graph::LinkUniqueId,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!("the MCP front end awaits the async op, never the blocking wrapper")
        }
        fn request_this_streams_shutdown(
            &self,
            _reason: &str,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!(
                "the control plane never asks a stream to shut down; `close_project` unloads it"
            )
        }
        fn request_a_function_node_promotion_out_of_the_islands_of_the_reader_it_crashed(
            &self,
            _function_node_id: &::streamlib::sdk::graph::NodeRunId,
            _reader_id: &::streamlib::sdk::graph::NodeRunId,
            _promotion_request_number: u64,
            _reason: &str,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!("only a reader's own host asks for a function node's promotion")
        }
    };
}

pub(crate) use graph_mutation_ops_record_the_call;
