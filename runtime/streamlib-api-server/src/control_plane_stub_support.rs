// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test scaffolding for the control plane's two front ends and for
//! the tests that assert on what it logs.

/// The exposed-port subscription a control-plane stub never reaches: the
/// control plane does not read an exposed port's channel.
macro_rules! the_exposed_port_subscription_is_unreachable {
    ($surface:literal) => {
        fn subscribe_to_an_exposed_output_port(
            &self,
            _node_name: String,
            _port_name: String,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<
                ::streamlib::sdk::runtime::ExposedOutputPortSubscription,
            >,
        > {
            unreachable!(concat!(
                "the control plane serves no exposed-port subscription ",
                $surface
            ))
        }
    };
}
pub(crate) use the_exposed_port_subscription_is_unreachable;

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
        fn add_processor_async(
            &self,
            _spec: ::streamlib::sdk::processors::ProcessorSpec,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::NodeInTheGraph>,
        > {
            unreachable!(concat!(
                "the control plane serves no processor-creation ",
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
        fn remove_processor_async(
            &self,
            _processor_id: ::streamlib::sdk::graph::ProcessorUniqueId,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            unreachable!(concat!(
                "the control plane serves no processor-removal ",
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
        fn add_processor(
            &self,
            _spec: ::streamlib::sdk::processors::ProcessorSpec,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::ProcessorUniqueId> {
            unreachable!(concat!(
                "the control plane serves no processor-creation ",
                $surface
            ))
        }
        fn remove_processor(
            &self,
            _processor_id: &::streamlib::sdk::graph::ProcessorUniqueId,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!(concat!(
                "the control plane serves no processor-removal ",
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
                "the control plane never asks a stream to shut down; `stop_stream` unloads it"
            )
        }
    };
}

pub(crate) use graph_mutation_ops_are_unreachable;

/// One graph mutation a front-end stub saw, with the arguments it was handed.
#[derive(Debug)]
pub(crate) enum RecordedGraphMutation {
    AddProcessor(::streamlib::sdk::processors::ProcessorSpec),
    RemoveProcessor(::streamlib::sdk::graph::ProcessorUniqueId),
    Connect(
        ::streamlib::sdk::graph::OutputLinkPortRef,
        ::streamlib::sdk::graph::InputLinkPortRef,
    ),
    Disconnect(::streamlib::sdk::graph::LinkUniqueId),
}

/// The graph mutations a front end handed the runtime, in call order.
pub(crate) type RecordedGraphMutations =
    ::std::sync::Arc<::parking_lot::Mutex<Vec<RecordedGraphMutation>>>;

/// The id the stub answers every `add_node` with.
pub(crate) const STUB_ADDED_PROCESSOR_ID: &str = "stub-added-processor";

/// The name the stub answers an `add_node` that named none with.
pub(crate) const STUB_ADDED_NODE_NAME: &str = "stub-added-node";

/// The one node name the stub's lookup refuses; every other resolves to its
/// cast followed by [`STUB_NODE_ID_SUFFIX`].
pub(crate) const STUB_ABSENT_NODE_NAME: &str = "absent";

/// What the stub's lookup appends to a node's name to make its id.
pub(crate) const STUB_NODE_ID_SUFFIX: &str = "-id";

/// The id the stub answers every `connect` with.
pub(crate) const STUB_CREATED_LINK_ID: &str = "stub-created-link";

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
/// version, build id, uptime and own-log folder.
pub(crate) fn the_stub_runtimes_description()
-> ::streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult {
    ::streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult {
        version: "0.0.0-stub".to_owned(),
        build_id: "0.0.0-stub+unknown.stub".to_owned(),
        up_for_ms: 266_400_000,
        runtime_log_directory: Some(::std::path::PathBuf::from("/stub-state/logs")),
    }
}

/// The one stream a stub runtime loads: the stub itself.
pub(crate) const STUB_STREAM_ID: &str = "stub-stream";

/// The tag of the stub's one load, the same for every call in this process.
pub(crate) fn the_stub_streams_tag() -> ::streamlib::sdk::runtime::LoadedStreamTag {
    static STUB_STREAM_TAG: ::std::sync::OnceLock<::streamlib::sdk::runtime::LoadedStreamTag> =
        ::std::sync::OnceLock::new();
    *STUB_STREAM_TAG.get_or_init(|| {
        ::streamlib::sdk::runtime::LoadedStreamTag::next_in_this_process()
            .expect("a test process has a stream tag left")
    })
}

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
/// record; it lists as running, and every stream action is refused.
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
            fn node_catalog_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<
                Vec<::streamlib::sdk::descriptors::ProcessorDescriptor>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok(::streamlib::sdk::processors::PROCESSOR_REGISTRY.list_registered())
            }
            fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
                &self,
                stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<
                Vec<::streamlib::sdk::descriptors::ProcessorDescriptor>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_id,
                )?;
                Ok(Vec::new())
            }
            fn ids_of_the_loaded_streams(&self) -> Vec<String> {
                vec![$crate::control_plane_stub_support::STUB_STREAM_ID.to_string()]
            }
            fn run_stream(
                &self,
                _request: ::streamlib::sdk::runtime::RunStreamRequest,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamRunOutcome> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "run_stream",
                ))
            }
            fn run_project(
                &self,
                _request: ::streamlib::sdk::runtime::RunProjectRequest,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "run_project",
                ))
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
            fn list_project_streams(
                &self,
                _project_directory: &::std::path::Path,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::ProjectStreamsListing>
            {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "list_project_streams",
                ))
            }
            fn stop_stream(
                &self,
                _stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "stop_stream",
                ))
            }
            fn start_stream(
                &self,
                _stream_id: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamActionQueued> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "start_stream",
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
            fn rename_stream(
                &self,
                _stream_id: &str,
                _display_name: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamRenameOutcome> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "rename_stream",
                ))
            }
            fn list_streams(&self) -> Vec<::streamlib::sdk::runtime::StreamListing> {
                vec![::streamlib::sdk::runtime::StreamListing {
                    stream_id: $crate::control_plane_stub_support::STUB_STREAM_ID.to_string(),
                    display_name: $crate::control_plane_stub_support::STUB_STREAM_ID.to_string(),
                    stream_location: None,
                    state: ::streamlib::sdk::runtime::StreamListingState::Running,
                    node_count: None,
                    failed_because: None,
                    stream_action_under_way: None,
                }]
            }
            fn expose_port(
                &self,
                _stream_id: &str,
                _node: &str,
                _port: &str,
                _level: ::streamlib::sdk::graph::OutputPortExposureLevel,
            ) -> ::streamlib::sdk::error::Result<
                ::streamlib::sdk::runtime::OutputPortExposureOutcome,
            > {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "expose_port",
                ))
            }
            fn every_loaded_stream_and_the_output_ports_it_exposes(
                &self,
            ) -> Vec<::streamlib::sdk::runtime::LoadedStreamAndTheOutputPortsItExposes> {
                Vec::new()
            }
        }
    };
}

pub(crate) use a_stub_runtime_loading_this_stub_as_its_only_stream;

/// What a stub runtime answers an `add_node` with when the test has armed a
/// refusal, standing in for an engine-side one — a name already taken, an
/// unknown class.
pub(crate) type ArmedAddProcessorRefusal = ::std::sync::Arc<::parking_lot::Mutex<Option<String>>>;

/// Implement the four async graph-mutating [`RuntimeOperations`] methods by
/// recording the call on a `recorded_graph_mutations` field and answering a
/// fixed id, so a front-end test can assert its tool reached the matching op
/// with the arguments the caller sent. An `armed_add_processor_refusal` the
/// test filled makes the add refuse instead, so a front end's handling of an
/// engine refusal is testable without an engine. The blocking wrappers and the
/// one stream's own shutdown request stay unreachable: a front end awaits, it
/// never blocks a worker, and it asks no stream to shut down.
macro_rules! graph_mutation_ops_record_the_call {
    () => {
        fn add_processor_async(
            &self,
            spec: ::streamlib::sdk::processors::ProcessorSpec,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::NodeInTheGraph>,
        > {
            let name = match spec.display_name.as_deref() {
                Some(requested_name) => {
                    ::streamlib::sdk::graph::cast_exposed_name_to_url_safe(requested_name)
                        .map(|cast| cast.into_owned())
                }
                None => Ok($crate::control_plane_stub_support::STUB_ADDED_NODE_NAME.to_string()),
            };
            self.recorded_graph_mutations.lock().push(
                $crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec),
            );
            let armed_refusal = self.armed_add_processor_refusal.lock().clone();
            let answers_after = self.add_processor_answers_after;
            Box::pin(async move {
                if let Some(answers_after) = answers_after {
                    ::tokio::time::sleep(answers_after).await;
                }
                match armed_refusal {
                    Some(refusal) => Err(::streamlib::sdk::error::Error::Configuration(refusal)),
                    None => Ok(::streamlib::sdk::runtime::NodeInTheGraph {
                        processor_id: ::streamlib::sdk::graph::ProcessorUniqueId::from(
                            $crate::control_plane_stub_support::STUB_ADDED_PROCESSOR_ID,
                        ),
                        name: name?,
                    }),
                }
            })
        }
        fn the_node_named(
            &self,
            node_name: &str,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::NodeInTheGraph> {
            let cast = ::streamlib::sdk::graph::cast_exposed_name_to_url_safe(node_name)?;
            if cast == $crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME {
                return Err(::streamlib::sdk::error::Error::ProcessorNotFound(format!(
                    "no node on this runtime is named {node_name:?}"
                )));
            }
            Ok(::streamlib::sdk::runtime::NodeInTheGraph {
                processor_id: ::streamlib::sdk::graph::ProcessorUniqueId::from(format!(
                    "{cast}{}",
                    $crate::control_plane_stub_support::STUB_NODE_ID_SUFFIX
                )),
                name: cast.into_owned(),
            })
        }
        fn remove_processor_async(
            &self,
            processor_id: ::streamlib::sdk::graph::ProcessorUniqueId,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            self.recorded_graph_mutations.lock().push(
                $crate::control_plane_stub_support::RecordedGraphMutation::RemoveProcessor(
                    processor_id,
                ),
            );
            Box::pin(async { Ok(()) })
        }
        fn connect_async(
            &self,
            from: ::streamlib::sdk::graph::OutputLinkPortRef,
            to: ::streamlib::sdk::graph::InputLinkPortRef,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkUniqueId>,
        > {
            self.recorded_graph_mutations
                .lock()
                .push($crate::control_plane_stub_support::RecordedGraphMutation::Connect(from, to));
            Box::pin(async {
                Ok(::streamlib::sdk::graph::LinkUniqueId::from(
                    $crate::control_plane_stub_support::STUB_CREATED_LINK_ID,
                ))
            })
        }
        fn disconnect_async(
            &self,
            link_id: ::streamlib::sdk::graph::LinkUniqueId,
        ) -> ::streamlib::sdk::runtime::BoxFuture<'_, ::streamlib::sdk::error::Result<()>> {
            self.recorded_graph_mutations.lock().push(
                $crate::control_plane_stub_support::RecordedGraphMutation::Disconnect(link_id),
            );
            Box::pin(async { Ok(()) })
        }
        fn add_processor(
            &self,
            _spec: ::streamlib::sdk::processors::ProcessorSpec,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::ProcessorUniqueId> {
            unreachable!("the MCP front end awaits the async op, never the blocking wrapper")
        }
        fn remove_processor(
            &self,
            _processor_id: &::streamlib::sdk::graph::ProcessorUniqueId,
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
                "the control plane never asks a stream to shut down; `stop_stream` unloads it"
            )
        }
    };
}

pub(crate) use graph_mutation_ops_record_the_call;

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

/// The real router over `operations_on_the_loaded_streams` and
/// `relay_instances`, served on a local API socket at `local_api_socket_path`
/// until the returned server is dropped.
pub(crate) fn serve_the_control_plane_router_at(
    operations_on_the_loaded_streams: ::std::sync::Arc<
        dyn ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime,
    >,
    relay_instances: ::std::sync::Arc<
        dyn ::streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine,
    >,
    local_api_socket_path: &::std::path::Path,
) -> crate::local_api_socket::RunningLocalApiSocketServer {
    crate::local_api_socket::bind_local_api_socket(local_api_socket_path)
        .expect("the local API socket binds in a fresh directory")
        .serve_router(
            |local_api_stopping_token| {
                crate::handlers::build_router(
                    operations_on_the_loaded_streams,
                    relay_instances,
                    local_api_stopping_token,
                )
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
    /// Over `operations_on_the_loaded_streams`, its relays a
    /// [`RelayInstancesOfThisMachineStub`].
    pub(crate) fn over(
        operations_on_the_loaded_streams: ::std::sync::Arc<
            dyn ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime,
        >,
    ) -> Self {
        Self::over_with_relay_instances(
            operations_on_the_loaded_streams,
            RelayInstancesOfThisMachineStub::shared(),
        )
    }

    /// Over `operations_on_the_loaded_streams` and `relay_instances`.
    pub(crate) fn over_with_relay_instances(
        operations_on_the_loaded_streams: ::std::sync::Arc<
            dyn ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime,
        >,
        relay_instances: ::std::sync::Arc<
            dyn ::streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine,
        >,
    ) -> Self {
        let socket_directory = ::tempfile::tempdir().expect("a temp directory");
        let local_api_socket_path = socket_directory.path().join("local-api.sock");
        Self {
            running_server: Some(serve_the_control_plane_router_at(
                operations_on_the_loaded_streams,
                relay_instances,
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

/// The adapter kind the relay stub catalogs beside the local one.
pub(crate) const STUB_RELAY_ADAPTER_KIND: &str = "stub-provider";

/// The name people read for the stub's hosted adapter.
pub(crate) const STUB_RELAY_ADAPTER_DISPLAY_NAME: &str = "Stub Provider";

/// The icon both stub adapters carry: a one-rectangle SVG.
const STUB_RELAY_ADAPTER_DISPLAY_ICON_SVG: &str =
    r#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#;

/// The `data:` URI the relay stub catalogs for each adapter's icon.
pub(crate) fn stub_relay_adapter_display_icon_data_uri() -> String {
    ::streamlib_relay_adapter_sdk::RelayAdapterCatalogEntry::display_icon_data_uri_of_the_svg(
        STUB_RELAY_ADAPTER_DISPLAY_ICON_SVG,
    )
}

/// One call the relay stub took, in its own words, secrets and all.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RelayInstancesStubCall {
    Added {
        adapter: String,
        settings: ::serde_json::Map<String, ::serde_json::Value>,
    },
    Configured {
        relay_id: String,
        settings: ::serde_json::Map<String, ::serde_json::Value>,
    },
    Removed {
        relay_id: String,
    },
}

/// A machine's relays as the tools' tests need them: the local relay's two
/// built-in instances and a `stub-provider` adapter whose `token` is a secret,
/// each call recorded.
#[derive(Default)]
pub(crate) struct RelayInstancesOfThisMachineStub {
    pub(crate) calls: ::parking_lot::Mutex<Vec<RelayInstancesStubCall>>,
}

impl RelayInstancesOfThisMachineStub {
    /// A fresh stub, shared as the router takes it.
    pub(crate) fn shared()
    -> ::std::sync::Arc<dyn ::streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine> {
        ::std::sync::Arc::new(Self::default())
    }
}

const STUB_BUILT_IN_RELAY_IDS: [&str; 2] = ["local-loopback", "local-every-interface"];

impl ::streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine
    for RelayInstancesOfThisMachineStub
{
    fn relay_adapter_catalog(
        &self,
    ) -> Vec<::streamlib_relay_adapter_sdk::RelayAdapterCatalogEntry> {
        vec![
            ::streamlib_relay_adapter_sdk::RelayAdapterCatalogEntry {
                kind: "local".to_string(),
                display_name: "Local relay".to_string(),
                display_icon_data_uri: stub_relay_adapter_display_icon_data_uri(),
                doc: "The machine's own relay, built in.".to_string(),
                settings: vec![::streamlib_relay_adapter_sdk::RelayAdapterSettingField {
                    name: "port".to_string(),
                    field_type: ::streamlib_relay_adapter_sdk::RelaySettingFieldType::Number,
                    secret: false,
                    required: false,
                    doc: "The port the listener takes.".to_string(),
                    default: None,
                }],
            },
            ::streamlib_relay_adapter_sdk::RelayAdapterCatalogEntry {
                kind: STUB_RELAY_ADAPTER_KIND.to_string(),
                display_name: STUB_RELAY_ADAPTER_DISPLAY_NAME.to_string(),
                display_icon_data_uri: stub_relay_adapter_display_icon_data_uri(),
                doc: "A hosted relay standing in for a provider.".to_string(),
                settings: vec![
                    ::streamlib_relay_adapter_sdk::RelayAdapterSettingField {
                        name: "region".to_string(),
                        field_type: ::streamlib_relay_adapter_sdk::RelaySettingFieldType::Select {
                            options: vec!["us".to_string(), "eu".to_string()],
                        },
                        secret: false,
                        required: false,
                        doc: "The region the relay runs in.".to_string(),
                        default: None,
                    },
                    ::streamlib_relay_adapter_sdk::RelayAdapterSettingField {
                        name: "token".to_string(),
                        field_type: ::streamlib_relay_adapter_sdk::RelaySettingFieldType::String,
                        secret: true,
                        required: true,
                        doc: "The provider's API token.".to_string(),
                        default: None,
                    },
                ],
            },
        ]
    }

    fn add_relay(
        &self,
        adapter: &str,
        settings: ::serde_json::Map<String, ::serde_json::Value>,
    ) -> Result<
        ::streamlib_relay_adapter_sdk::RelayInstanceId,
        ::streamlib_relay_adapter_sdk::RelayInstanceControlRefusal,
    > {
        if adapter != STUB_RELAY_ADAPTER_KIND {
            return Err(
                ::streamlib_relay_adapter_sdk::RelayInstanceControlRefusal::UnknownAdapter {
                    adapter: adapter.to_string(),
                    known: format!("`local`, `{STUB_RELAY_ADAPTER_KIND}`"),
                },
            );
        }
        self.calls.lock().push(RelayInstancesStubCall::Added {
            adapter: adapter.to_string(),
            settings,
        });
        Ok(::streamlib_relay_adapter_sdk::RelayInstanceId(format!(
            "{STUB_RELAY_ADAPTER_KIND}-1"
        )))
    }

    fn configure_relay(
        &self,
        relay_id: &str,
        settings: ::serde_json::Map<String, ::serde_json::Value>,
    ) -> Result<(), ::streamlib_relay_adapter_sdk::RelayInstanceControlRefusal> {
        self.calls.lock().push(RelayInstancesStubCall::Configured {
            relay_id: relay_id.to_string(),
            settings,
        });
        Ok(())
    }

    fn remove_relay(
        &self,
        relay_id: &str,
    ) -> Result<(), ::streamlib_relay_adapter_sdk::RelayInstanceControlRefusal> {
        if STUB_BUILT_IN_RELAY_IDS.contains(&relay_id) {
            return Err(
                ::streamlib_relay_adapter_sdk::RelayInstanceControlRefusal::BuiltInRelayIsNeverRemoved {
                    relay_id: relay_id.to_string(),
                },
            );
        }
        self.calls.lock().push(RelayInstancesStubCall::Removed {
            relay_id: relay_id.to_string(),
        });
        Ok(())
    }

    fn list_relays(
        &self,
    ) -> Vec<::streamlib_runtime_client_contract::local_api_wire_contract::ListedRelayInstance>
    {
        use ::streamlib_runtime_client_contract::local_api_wire_contract::{
            ListedRelayConnectionState, ListedRelayInstance, ListedRelaySecretPresence,
        };
        let mut listed: Vec<ListedRelayInstance> = STUB_BUILT_IN_RELAY_IDS
            .iter()
            .map(|relay_id| ListedRelayInstance {
                relay_id: relay_id.to_string(),
                adapter: "local".to_string(),
                built_in: true,
                settings: ::serde_json::Map::new(),
                secrets: Default::default(),
                connection_state: ListedRelayConnectionState::Joined,
            })
            .collect();
        listed.push(ListedRelayInstance {
            relay_id: format!("{STUB_RELAY_ADAPTER_KIND}-1"),
            adapter: STUB_RELAY_ADAPTER_KIND.to_string(),
            built_in: false,
            settings: ::serde_json::json!({"region": "us"})
                .as_object()
                .expect("an object")
                .clone(),
            secrets: [("token".to_string(), ListedRelaySecretPresence::Set)].into(),
            connection_state: ListedRelayConnectionState::Joining,
        });
        listed
    }
}

/// One HTTP/1.1 GET over the socket at `local_api_socket_path`: its status
/// line and body.
pub(crate) async fn http_get_over_the_local_api_socket(
    local_api_socket_path: &std::path::Path,
    request_target: &str,
) -> (String, String) {
    let mut connection = tokio::net::UnixStream::connect(local_api_socket_path)
        .await
        .expect("the local API socket accepts a connection");
    ::tokio::io::AsyncWriteExt::write_all(
        &mut connection,
        format!("GET {request_target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
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
