// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Shared test scaffolding for the control plane's two front ends and for
//! the tests that assert on what it logs.

/// The viewer a control-plane stub never opens.
macro_rules! every_reserved_slot_reader_is_unreachable {
    ($surface:literal) => {
        fn open_a_viewer_of_an_output_port(
            &self,
            node_id: String,
            port_name: String,
            _process_id_of_the_viewer: Option<u32>,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::OutputPortViewerHeldOpen>,
        > {
            Box::pin(async move {
                Err(::streamlib::sdk::error::Error::Link(format!(
                    "the stub stream runs no node `{node_id}`, so port `{port_name}` has \
                     nothing to view"
                )))
            })
        }
        fn the_running_view_node_a_panel_writes(
            &self,
            node_id: String,
            port_name: String,
            _value: ::serde_json::Value,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<::uuid::Uuid>>
        {
            Box::pin(async move {
                Err(::streamlib::sdk::error::Error::Link(format!(
                    "the stub stream runs no view node `{node_id}`, so its output `{port_name}` \
                     takes no value"
                )))
            })
        }

        fn the_top_level_graph_input_declaring(
            &self,
            input_name: String,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<
                ::streamlib::sdk::runtime::TopLevelGraphInputDeclaringAStreamInput,
            >,
        > {
            Box::pin(async move {
                Err(::streamlib::sdk::error::Error::NodeNotFound(format!(
                    "the stub stream has no input `{input_name}`; its inputs are none"
                )))
            })
        }
    };
}
pub(crate) use every_reserved_slot_reader_is_unreachable;

/// Implement every graph-mutating [`RuntimeOperations`] method, and the one
/// stream's own shutdown request, as `unreachable!`, naming `$surface`
/// (`"route"` / `"tool"`) in each graph-mutation panic.
///
/// The trait still declares these — the runtime API is not what the pivot
/// changed — but no route and no tool may reach one. A stub that answered them
/// permissively would let a regrown surface pass its test; this makes it fail.
///
/// Both front ends' stubs expand this so adding a `RuntimeOperations` method is
/// one edit rather than two silently-diverging ones. The panic fires on call
/// rather than on poll, because these return `BoxFuture` from a non-async fn.
macro_rules! graph_mutation_ops_are_unreachable {
    ($surface:literal) => {
        fn add_node_async(
            &self,
            _spec: ::streamlib::sdk::nodes::NodeSpec,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::NodeInTheGraph>,
        > {
            unreachable!(concat!(
                "the control plane serves no node-creation ",
                $surface
            ))
        }
        fn the_node_named(
            &self,
            _node_name: &str,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::NodeInTheGraph> {
            unreachable!(concat!(
                "the control plane serves no node lookup ",
                $surface
            ))
        }
        fn remove_node_async(
            &self,
            _node_run_id: ::streamlib::sdk::graph::NodeRunId,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            unreachable!(concat!(
                "the control plane serves no node-removal ",
                $surface
            ))
        }
        fn connect_async(
            &self,
            _from: ::streamlib::sdk::graph::OutputLinkPortRef,
            _to: ::streamlib::sdk::graph::InputLinkPortRef,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkUniqueId>,
        > {
            unreachable!(concat!("the control plane serves no connect ", $surface))
        }
        fn disconnect_async(
            &self,
            _link_id: ::streamlib::sdk::graph::LinkUniqueId,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            unreachable!(concat!("the control plane serves no disconnect ", $surface))
        }
        fn reconcile_each_stream_fragment_to_the_stream_it_references_live(
            &self,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn what_a_live_connect_makes_of_the_link(
            &self,
            _link: &::streamlib_project_repository::StreamGraphDocumentLink,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::WhatALiveConnectMakesOfALink> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn the_verdict_of_a_link_between_two_running_nodes(
            &self,
            _link: &::streamlib_project_repository::StreamGraphDocumentLink,
        ) -> ::std::option::Option<::streamlib::sdk::descriptors::PinLinkVerdict> {
            None
        }
        fn the_verdict_of_a_link_between_two_node_types(
            &self,
            _from: ::streamlib::sdk::runtime::PortOfANodeOfAType<'_>,
            _to: ::streamlib::sdk::runtime::PortOfANodeOfAType<'_>,
        ) -> ::std::option::Option<::streamlib::sdk::descriptors::PinLinkVerdict> {
            None
        }
        fn what_a_live_connect_makes_of_links_with_node_types_ports<'a>(
            &'a self,
            _held: &'a ::streamlib_project_repository::StreamGraphDocumentLinkEnd,
            _held_direction: ::streamlib::sdk::error::PortDirection,
            _node_types_ports: &'a [::streamlib::sdk::runtime::ANodeTypesPort<'a>],
        ) -> ::streamlib::sdk::runtime::BoxFuture<'a, ::streamlib::sdk::error::Result<::std::vec::Vec<::streamlib::sdk::error::Result<::streamlib::sdk::runtime::WhatALiveConnectMakesOfALink>>>> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn what_a_connect_makes_of_each_link_at_a_placed_level(
            &self,
            _level: &::streamlib_project_repository::StreamGraphDocument,
            _links: &[::streamlib_project_repository::StreamGraphDocumentLink],
        ) -> ::std::vec::Vec<::streamlib::sdk::error::Result<::streamlib::sdk::runtime::WhatALiveConnectMakesOfALink>> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn the_name_a_node_added_with_none_takes(
            &self,
            _node_type: &str,
            _names_at_the_level: &::std::collections::HashSet<String>,
        ) -> ::streamlib::sdk::error::Result<String> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn refuse_a_node_a_live_add_would_refuse(
            &self,
            _node_type: &str,
            _node_name: &str,
            _input_values: &::serde_json::Map<String, ::serde_json::Value>,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn the_ports_a_node_type_declares(
            &self,
            _node_type: &str,
        ) -> ::std::option::Option<::streamlib::sdk::graph::GraphNodePorts> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn the_pin_groups_a_node_type_declares(
            &self,
            _node_type: &str,
        ) -> ::std::vec::Vec<::streamlib::sdk::descriptors::PinGroupDescriptor> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn refuse_an_input_value_its_input_does_not_take(
            &self,
            _node_type: &::streamlib::sdk::descriptors::NodeClassImportPath,
            _node_name: &str,
            _input_values_after_the_set: &::serde_json::Map<String, ::serde_json::Value>,
            _input: &str,
            _member: ::std::option::Option<&str>,
            _value: ::std::option::Option<&::serde_json::Value>,
        ) -> ::std::result::Result<(), ::streamlib::sdk::graph::input_values::InputValueRefusal> {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn set_input_values_live(
            &self,
            _sets: &[::streamlib::sdk::graph::input_values::InputValueToSetLive],
        ) -> ::std::result::Result<
            ::streamlib::sdk::graph::input_values::InputValuesSetLive,
            ::streamlib::sdk::graph::input_values::LiveInputValueSetRefusal,
        > {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn hold_an_edit_its_document_holds_not_live(
            &self,
            _graph_operation: &::streamlib_project_repository::StreamGraphOperation,
            _refusal: &str,
        ) {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn rebuild_each_node_a_setting_read_at_setup_changed(
            &self,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::std::vec::Vec<::uuid::Uuid>>,
        > {
            ::std::boxed::Box::pin(::std::future::ready(Ok(::std::vec::Vec::new())))
        }
        fn apply_a_stream_graph_operation_live(
            &self,
            _graph_operation: ::streamlib_project_repository::StreamGraphOperation,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib_project_repository::StreamGraphOperation>,
        > {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn open_a_graph_edit_batch(
            self: ::std::sync::Arc<Self>,
        ) -> ::streamlib::sdk::error::Result<
            ::std::boxed::Box<dyn ::streamlib::sdk::runtime::GraphEditBatch>,
        > {
            unreachable!(concat!("the control plane serves no graph edit ", $surface))
        }
        fn add_node(
            &self,
            _spec: ::streamlib::sdk::nodes::NodeSpec,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::NodeRunId> {
            unreachable!(concat!(
                "the control plane serves no node-creation ",
                $surface
            ))
        }
        fn remove_node(
            &self,
            _node_run_id: &::streamlib::sdk::graph::NodeRunId,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!(concat!(
                "the control plane serves no node-removal ",
                $surface
            ))
        }
        fn connect(
            &self,
            _from: ::streamlib::sdk::graph::OutputLinkPortRef,
            _to: ::streamlib::sdk::graph::InputLinkPortRef,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkUniqueId> {
            unreachable!(concat!("the control plane serves no connect ", $surface))
        }
        fn disconnect(
            &self,
            _link_id: &::streamlib::sdk::graph::LinkUniqueId,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!(concat!("the control plane serves no disconnect ", $surface))
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

pub(crate) use graph_mutation_ops_are_unreachable;

/// The graph edits a front end applied live, in call order.
pub(crate) type RecordedGraphMutations = ::std::sync::Arc<
    ::parking_lot::Mutex<Vec<::streamlib_project_repository::StreamGraphOperation>>,
>;

/// The name the stub answers an `add_node` that named none with.
pub(crate) const STUB_ADDED_NODE_NAME: &str = "stub-added-node";

/// The name of the machine the stub runtime runs on, and the top-level
/// `machine` a test arms its graph with — the first part of an address.
pub(crate) const STUB_MACHINE_NAME: &str = "stub-machine";

/// The machine the stub runtime answers `this_machines_name` with, named
/// [`STUB_MACHINE_NAME`].
pub(crate) fn the_stub_machine() -> &'static ::streamlib::sdk::runtime::MachineName {
    static STUB_MACHINE: ::std::sync::OnceLock<::streamlib::sdk::runtime::MachineName> =
        ::std::sync::OnceLock::new();
    STUB_MACHINE.get_or_init(|| {
        ::streamlib::sdk::runtime::MachineName::from_a_hostname(STUB_MACHINE_NAME)
            .expect("the stub machine's name names something")
    })
}

/// The description a stub runtime answers `describe_runtime` with: a fixed
/// version, build id, uptime and own-log folder, and [`STUB_MACHINE_NAME`].
pub(crate) fn the_stub_runtimes_description()
-> ::streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult {
    ::streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult {
        version: "0.0.0-stub".to_owned(),
        build_id: "0.0.0-stub+unknown.stub".to_owned(),
        up_for_ms: 266_400_000,
        runtime_log_directory: Some(::std::path::PathBuf::from("/stub-state/logs")),
        machine: the_stub_machine().name().to_owned(),
    }
}

/// The audio capture devices a stub runtime lists: a microphone, the default output's monitor
/// and that output's own monitor, on a backend named `stub-audio`.
pub(crate) fn the_stub_machines_audio_capture_devices()
-> ::streamlib_runtime_client_contract::local_api_wire_contract::ListAudioCaptureDevicesToolResult {
    use ::streamlib_runtime_client_contract::local_api_wire_contract::{
        AudioCaptureDeviceKindOnTheWire, AudioCaptureDeviceOnTheWire,
        DEFAULT_OUTPUT_MONITOR_DEVICE_ID, ListAudioCaptureDevicesToolResult,
    };
    let a_device = |id: &str, name: &str, kind| AudioCaptureDeviceOnTheWire {
        id: id.to_owned(),
        name: name.to_owned(),
        kind,
    };
    ListAudioCaptureDevicesToolResult {
        audio_backend: "stub-audio".to_owned(),
        devices: vec![
            a_device(
                "stub-microphone",
                "Stub Microphone",
                AudioCaptureDeviceKindOnTheWire::Input,
            ),
            a_device(
                DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
                "What the default output plays (now Monitor of Stub Speakers)",
                AudioCaptureDeviceKindOnTheWire::OutputMonitor,
            ),
            a_device(
                "stub-speakers.monitor",
                "Monitor of Stub Speakers",
                AudioCaptureDeviceKindOnTheWire::OutputMonitor,
            ),
        ],
    }
}

/// The one stream a stub runtime loads: the stub itself.
pub(crate) const STUB_STREAM_ID: &str = "stub-stream";

/// The stream asset the stub's one stream runs.
pub(crate) const STUB_STREAM_ASSET: ::uuid::Uuid =
    ::uuid::Uuid::from_u128(0x57ab_0000_0000_0000_0000_0000_0001);

/// The project the stub's one stream is of.
pub(crate) const STUB_PROJECT: ::uuid::Uuid =
    ::uuid::Uuid::from_u128(0x57ab_0000_0000_0000_0000_0000_0002);

/// Where the stub's one stream's project lives.
pub(crate) const STUB_PROJECT_DIRECTORY: &str = "/stub-project";

/// Refuse a call naming a stream other than [`STUB_STREAM_ID`], in the
/// engine's words.
pub(crate) fn refuse_a_stream_the_stub_runtime_does_not_load(
    stream_id: &str,
) -> ::streamlib::sdk::error::Result<()> {
    match stream_id {
        STUB_STREAM_ID => Ok(()),
        other => Err(::streamlib::sdk::error::Error::NotFound(format!(
            "no stream named `{other}` is loaded in this runtime. Loaded: {STUB_STREAM_ID}"
        ))),
    }
}

/// The identity the stub stream is loaded under, one for the whole test process.
pub(crate) fn the_stub_streams_loaded_identity() -> ::streamlib::sdk::pubsub::LoadedStreamIdentity {
    static THE_STUB_STREAMS_LOADED_IDENTITY: ::std::sync::OnceLock<
        ::streamlib::sdk::pubsub::LoadedStreamIdentity,
    > = ::std::sync::OnceLock::new();
    THE_STUB_STREAMS_LOADED_IDENTITY
        .get_or_init(|| ::streamlib::sdk::pubsub::LoadedStreamIdentity {
            runtime_id: STUB_MACHINE_NAME.to_owned(),
            stream_id: STUB_STREAM_ID.to_owned(),
            stream_tag: ::streamlib::sdk::runtime::LoadedStreamTag::next_in_this_process()
                .expect("a stream tag is left for the stub stream"),
        })
        .clone()
}

/// The refusal every stream action meets on a stub runtime, which loads its
/// one stream and no other.
pub(crate) fn the_stub_runtime_takes_no_stream_action(
    action: &str,
) -> ::streamlib::sdk::error::Error {
    ::streamlib::sdk::error::Error::Runtime(format!(
        "`{action}` was refused: the stub runtime loads `{STUB_STREAM_ID}` and takes no stream \
         action"
    ))
}

/// Implement [`OperationsOnTheStreamsLoadedInThisRuntime`] for a stub
/// `RuntimeOperations` type as a runtime described by
/// [`the_stub_runtimes_description`] that loads one stream, the stub:
/// every call naming it reaches a clone of the stub (whose state is shared
/// behind `Arc`s); the node catalog is the native registry's,
/// and its interpreter described nothing; the stub stream's log holds no
/// record; it lists as running and carries no sample, every stream action is refused, and, handed no uv, it locks
/// no project's requirements.
///
/// [`OperationsOnTheStreamsLoadedInThisRuntime`]: ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime
macro_rules! a_stub_runtime_loading_this_stub_as_its_only_stream {
    ($stub_type:ty) => {
        impl ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime for $stub_type {
            fn this_machines_name(&self) -> &::streamlib::sdk::runtime::MachineName {
                $crate::control_plane_stub_support::the_stub_machine()
            }
            fn describe_this_runtime(
                &self,
            ) -> ::streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult
            {
                $crate::control_plane_stub_support::the_stub_runtimes_description()
            }
            fn list_audio_capture_devices(
                &self,
            ) -> ::streamlib::sdk::error::Result<
                ::streamlib_runtime_client_contract::local_api_wire_contract::ListAudioCaptureDevicesToolResult,
            > {
                Ok($crate::control_plane_stub_support::the_stub_machines_audio_capture_devices())
            }
            fn runtime_operations_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<
                ::std::sync::Arc<dyn ::streamlib::sdk::runtime::RuntimeOperations>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok(::std::sync::Arc::new(self.clone()))
            }
            fn the_streams_to_read_or_edit(
                &self,
                stream_ids: &[String],
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamsToReadOrEdit<'_>>
            {
                let mut runtime_operations_of_each_stream = ::std::collections::HashMap::new();
                for stream_id in stream_ids {
                    $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                        stream_id,
                    )?;
                    runtime_operations_of_each_stream.insert(
                        stream_id.clone(),
                        ::std::sync::Arc::new(self.clone())
                            as ::std::sync::Arc<dyn ::streamlib::sdk::runtime::RuntimeOperations>,
                    );
                }
                Ok(::streamlib::sdk::runtime::StreamsToReadOrEdit::of_streams_each_loaded(
                    runtime_operations_of_each_stream,
                ))
            }
            fn loaded_stream_identity_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::pubsub::LoadedStreamIdentity> {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok($crate::control_plane_stub_support::the_stub_streams_loaded_identity())
            }
            fn node_catalog_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<
                Vec<::streamlib::sdk::descriptors::NodeDescriptor>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok(::streamlib::sdk::nodes::NODE_REGISTRY.list_registered())
            }
            fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<
                Vec<::streamlib::sdk::descriptors::NodeDescriptor>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok(Vec::new())
            }
            fn node_types_installed_modules_declare_in_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<
                Vec<(
                    ::streamlib::sdk::descriptors::NodeDescriptor,
                    ::streamlib::sdk::nodes::InstalledModuleOfANodeType,
                )>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok(Vec::new())
            }
            fn pin_conversions_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<
                ::std::sync::Arc<[::streamlib::sdk::descriptors::PinConversion]>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok(::streamlib::sdk::nodes::NODE_REGISTRY.pin_conversions())
            }
            fn run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
                generator: &str,
                _fragment_name: Option<&str>,
                _promoted_settings: &::serde_json::Map<String, ::serde_json::Value>,
            ) -> ::streamlib::sdk::error::Result<::serde_json::Value> {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Err(::streamlib::sdk::error::Error::Configuration(format!(
                    "graph generator `{generator}` did not run: the stub runtime's stream \
                     runs no interpreter"
                )))
            }
            fn ids_of_the_loaded_streams(&self) -> Vec<String> {
                vec![$crate::control_plane_stub_support::STUB_STREAM_ID.to_string()]
            }
            fn open_project(
                &self,
                _request: ::streamlib::sdk::runtime::ProjectOpenOrReloadRequest,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "open_project",
                ))
            }
            fn close_project(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "close_project",
                ))
            }
            fn start_playback(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "start_playback",
                ))
            }
            fn make_the_projects_venv_current(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "make_the_projects_venv_current",
                ))
            }
            fn reload_project(
                &self,
                _request: ::streamlib::sdk::runtime::ProjectOpenOrReloadRequest,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "reload_project",
                ))
            }
            fn create_project(
                &self,
                _request: ::streamlib::sdk::runtime::CreateProjectRequest,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::ProjectCreationQueued>
            {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "create_project",
                ))
            }
            fn list_samples(
                &self,
            ) -> ::streamlib_runtime_client_contract::local_api_wire_contract::ListSamplesToolResult
            {
                ::streamlib_runtime_client_contract::local_api_wire_contract::ListSamplesToolResult {
                    samples: ::std::vec::Vec::new(),
                }
            }
            fn describe_stream_action(
                &self,
                _action_id: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamAction> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "describe_stream_action",
                ))
            }
            fn cancel_stream_action(
                &self,
                _action_id: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamAction> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "cancel_stream_action",
                ))
            }
            fn stream_actions_under_way(&self) -> Vec<::streamlib::sdk::runtime::StreamAction> {
                Vec::new()
            }
            fn listen_for_each_stream_action_change(
                &self,
            ) -> ::tokio::sync::broadcast::Receiver<String> {
                ::tokio::sync::broadcast::channel(1).1
            }
            fn forget_the_stream_of_a_deleted_stream_asset(
                &self,
                _project: ::uuid::Uuid,
                _stream_asset: ::uuid::Uuid,
            ) -> ::streamlib::sdk::error::Result<Option<String>> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "delete_asset",
                ))
            }
            fn stop_playback(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "stop_playback",
                ))
            }
            fn remove_project(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "remove_project",
                ))
            }
            fn delete_project(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "delete_project",
                ))
            }
            fn pause_playback(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::ProjectPausedOrResumed>
            {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "pause_playback",
                ))
            }
            fn resume_playback(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::ProjectPausedOrResumed>
            {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "resume_playback",
                ))
            }
            fn show_the_stream_of_a_stream_asset_under(
                &self,
                _project: ::uuid::Uuid,
                _stream_asset: ::uuid::Uuid,
                _display_name: &str,
            ) -> Option<String> {
                None
            }
            fn load_the_stream_of_a_created_stream_asset(
                &self,
                _project_directory: &::std::path::Path,
                _stream_asset: ::uuid::Uuid,
            ) -> Option<::streamlib::sdk::runtime::StreamActionQueued> {
                None
            }
            fn bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
                &self,
                _project_directory: &::std::path::Path,
            ) -> Option<::streamlib::sdk::runtime::StreamActionQueued> {
                None
            }
            fn list_streams(&self) -> Vec<::streamlib::sdk::runtime::StreamListing> {
                vec![::streamlib::sdk::runtime::StreamListing {
                    stream_id: $crate::control_plane_stub_support::STUB_STREAM_ID.to_string(),
                    display_name: $crate::control_plane_stub_support::STUB_STREAM_ID.to_string(),
                    stream_location: Some(::streamlib::sdk::runtime::StreamLocation {
                        project_directory: ::std::path::PathBuf::from(
                            $crate::control_plane_stub_support::STUB_PROJECT_DIRECTORY,
                        ),
                        project: $crate::control_plane_stub_support::STUB_PROJECT,
                        stream_asset: $crate::control_plane_stub_support::STUB_STREAM_ASSET,
                    }),
                    state: ::streamlib::sdk::runtime::StreamListingState::Running,
                    node_count: None,
                    failed_because: None,
                    condition: ::streamlib_runtime_client_contract::local_api_wire_contract::StreamConditionOnTheWire::healthy(
                        ::streamlib_runtime_client_contract::local_api_wire_contract::StreamLoadOnTheWire::Loaded,
                        ::streamlib_runtime_client_contract::local_api_wire_contract::StreamPlaybackOnTheWire::Playing,
                    ),
                    capture_devices_whose_access_was_never_asked_for: Vec::new(),
                    stream_action_under_way: None,
                }]
            }
            fn set_stream_clock_scale(
                &self,
                _stream_id: &str,
                _scale: f64,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::context::StreamClockReading> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "set_stream_clock_scale",
                ))
            }
            fn lock_the_python_requirements_of_the_project_in(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::std::result::Result<(), String> {
                Ok(())
            }
            fn the_core_nodes_project_directory_this_runtime_carries(
                &self,
            ) -> ::std::option::Option<::std::path::PathBuf> {
                None
            }
            fn the_core_project_history_directory(
                &self,
            ) -> ::std::option::Option<::std::path::PathBuf> {
                None
            }
            fn record_first_party_package_moves_in(
                &self,
                _history: ::std::sync::Arc<
                    dyn ::streamlib::sdk::runtime::ProjectHistoryRecordingFirstPartyPackageMoves,
                >,
            ) {
            }
            fn check_a_projects_extension_code(
                &self,
                project_directory: &::std::path::Path,
                paths: ::std::option::Option<&[String]>,
            ) -> ::streamlib::sdk::runtime::ExtensionCodeCheck {
                ::streamlib::sdk::runtime::ExtensionCodeCheckersHeld::default()
                    .check_a_projects_extension_code(project_directory, paths, None)
            }
            fn a_template_from_the_extension_sdk_kit(
                &self,
                project_directory: &::std::path::Path,
                template_file: &str,
            ) -> ::std::result::Result<::streamlib::sdk::runtime::ExtensionSdkKitTemplate, String> {
                ::streamlib::sdk::runtime::ExtensionCodeCheckersHeld::default()
                    .a_template_from_the_kit(project_directory, template_file, None)
            }
        }
    };
}

pub(crate) use a_stub_runtime_loading_this_stub_as_its_only_stream;

mod graph_mutations_recorded_by_a_stub;
pub(crate) use graph_mutations_recorded_by_a_stub::{
    ArmedAddNodeRefusal, ArmedRefusalOfEachEditTheDocumentsHoldFirst,
    GraphEditsOfAStubAppliedOneByOne, RecordedEditsHeldNotLive, RecordedRebuildsAndTheArmedRefusal,
    graph_mutation_ops_record_the_call,
};

/// The target of every tracing span and event raised on the calling thread
/// while this is its default subscriber's layer.
///
/// Spans are captured alongside events because the control plane's request
/// trace levels a span that raises no event of its own.
#[derive(Clone, Default)]
pub(crate) struct CapturedTracingTargets(::std::sync::Arc<::parking_lot::Mutex<Vec<&'static str>>>);

impl CapturedTracingTargets {
    /// Run `raising_them` twice under an `env_filter_directives`-filtered
    /// subscriber, and answer with what the second run raised.
    ///
    /// `registry + EnvFilter + one recording layer` is the subscriber the
    /// engine installs for an app, so what this captures is what that app's
    /// stdout and JSONL would carry.
    ///
    /// Twice, because `tracing` caches each callsite's `Interest`
    /// process-globally, computed once from whichever thread first reaches it —
    /// and the rest of this crate's tests drive the control plane with no
    /// subscriber installed, which caches `never` for the very records a log
    /// assertion is about. The first run registers the callsites,
    /// `rebuild_interest_cache` recomputes them against this thread's filter,
    /// and the second run is the one answered. That rebuild rewrites
    /// process-global state, so every caller belongs under `#[serial]`.
    pub(crate) fn captured_from_the_second_of_two_runs(
        env_filter_directives: &str,
        raising_them: impl Fn(),
    ) -> Vec<&'static str> {
        use ::tracing_subscriber::layer::SubscriberExt;

        let captured = Self::default();
        let subscriber = ::tracing_subscriber::registry()
            .with(::tracing_subscriber::EnvFilter::new(env_filter_directives))
            .with(captured.clone());

        ::tracing::subscriber::with_default(subscriber, || {
            raising_them();
            ::tracing::callsite::rebuild_interest_cache();
            captured.0.lock().clear();
            raising_them();
        });

        let recorded = captured.0.lock();
        recorded.clone()
    }
}

impl<S: ::tracing::Subscriber> ::tracing_subscriber::layer::Layer<S> for CapturedTracingTargets {
    fn on_new_span(
        &self,
        span: &::tracing::span::Attributes<'_>,
        _id: &::tracing::span::Id,
        _context: ::tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.0.lock().push(span.metadata().target());
    }

    fn on_event(
        &self,
        event: &::tracing::Event<'_>,
        _context: ::tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.0.lock().push(event.metadata().target());
    }
}

/// The real router over `operations_on_the_loaded_streams`, served on a
/// local API socket at `local_api_socket_path` until the returned server is
/// dropped.
pub(crate) fn serve_the_control_plane_router_at(
    operations_on_the_loaded_streams: ::std::sync::Arc<
        dyn ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime,
    >,
    local_api_socket_path: &::std::path::Path,
) -> crate::local_api_socket::RunningLocalApiSocketServer {
    crate::local_api_socket::bind_local_api_socket(local_api_socket_path)
        .expect("the local API socket binds in a fresh directory")
        .serve_router(
            |local_api_stopping_token| {
                crate::handlers::build_router(
                    operations_on_the_loaded_streams,
                    local_api_stopping_token,
                )
                .local_api_socket_router
            },
            &::tokio::runtime::Handle::current(),
        )
        .expect("the bound local API socket is served")
}

/// The real router over a runtime's loaded streams, served on a local API
/// socket in a fresh temp directory for as long as this lives.
pub(crate) struct LocalApiServedOnAFreshSocket {
    pub(crate) local_api_socket_path: ::std::path::PathBuf,
    running_server: Option<crate::local_api_socket::RunningLocalApiSocketServer>,
    _socket_directory: ::tempfile::TempDir,
}

impl LocalApiServedOnAFreshSocket {
    /// Over `operations_on_the_loaded_streams`.
    pub(crate) fn over(
        operations_on_the_loaded_streams: ::std::sync::Arc<
            dyn ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime,
        >,
    ) -> Self {
        let socket_directory = ::tempfile::tempdir().expect("a temp directory");
        let local_api_socket_path = socket_directory.path().join("local-api.sock");
        Self {
            running_server: Some(serve_the_control_plane_router_at(
                operations_on_the_loaded_streams,
                &local_api_socket_path,
            )),
            local_api_socket_path,
            _socket_directory: socket_directory,
        }
    }

    /// Stop serving, as the node does when it stops.
    pub(crate) fn stop_serving(&mut self) {
        drop(self.running_server.take());
    }
}

/// Send `request_head` over `stream` and read the response head back, byte by
/// byte so nothing an upgraded protocol streams after it is consumed.
pub(crate) async fn response_head_over_the_socket(
    stream: &mut ::tokio::net::UnixStream,
    request_head: &str,
) -> String {
    use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

    stream.write_all(request_head.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

/// One HTTP/1.1 POST of `request_body` over the socket at
/// `local_api_socket_path`: its status line and body.
#[cfg(feature = "machine-directories-under-a-test-root")]
pub(crate) async fn http_post_over_the_local_api_socket(
    local_api_socket_path: &std::path::Path,
    request_target: &str,
    request_body: &str,
) -> (String, String) {
    let mut connection = tokio::net::UnixStream::connect(local_api_socket_path)
        .await
        .expect("the local API socket accepts a connection");
    ::tokio::io::AsyncWriteExt::write_all(
        &mut connection,
        format!(
            "POST {request_target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Length: {}\r\n\r\n{request_body}",
            request_body.len()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut answer = Vec::new();
    ::tokio::io::AsyncReadExt::read_to_end(&mut connection, &mut answer)
        .await
        .unwrap();
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let (head, body) = answer
        .split_once("\r\n\r\n")
        .expect("a complete response head");
    (
        head.lines().next().unwrap_or_default().to_string(),
        body.to_string(),
    )
}
