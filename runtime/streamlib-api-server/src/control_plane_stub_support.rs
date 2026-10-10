// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test scaffolding for the control plane's two front ends and for
//! the tests that assert on what it logs.

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
        fn this_runtimes_name(&self) -> &str {
            $crate::control_plane_stub_support::STUB_RUNTIME_NAME
        }
        fn request_this_streams_shutdown(
            &self,
            _reason: &str,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!(
                "the control plane asks for the shutdown of every loaded stream, never of one"
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

/// The name the stub runtime answers `this_runtimes_name` with, and the
/// top-level `runtime_name` a test arms its graph with — the first part of a
/// tap's channel.
pub(crate) const STUB_RUNTIME_NAME: &str = "stub-runtime";

/// The one stream a stub runtime loads: the stub itself.
pub(crate) const STUB_STREAM_NAME: &str = "stub-stream";

/// The topic the stub stream's events publish on.
pub(crate) const STUB_STREAM_EVENT_TOPIC: &str = "stream:stub-runtime-id/stub-stream#1";

/// Refuse a call naming a stream other than [`STUB_STREAM_NAME`], in the
/// engine's words; `None` names the stub stream, the only one loaded.
pub(crate) fn refuse_a_stream_the_stub_runtime_does_not_load(
    stream_name: Option<&str>,
) -> ::streamlib::sdk::error::Result<()> {
    match stream_name {
        None | Some(STUB_STREAM_NAME) => Ok(()),
        Some(other) => Err(::streamlib::sdk::error::Error::NotFound(format!(
            "no stream named `{other}` is loaded in this runtime. Loaded: {STUB_STREAM_NAME}"
        ))),
    }
}

/// The refusal every stream action meets on a stub runtime, which loads its
/// one stream and no other.
pub(crate) fn the_stub_runtime_takes_no_stream_action(
    action: &str,
) -> ::streamlib::sdk::error::Error {
    ::streamlib::sdk::error::Error::Runtime(format!(
        "`{action}` was refused: the stub runtime loads `{STUB_STREAM_NAME}` and takes no stream \
         action"
    ))
}

/// Implement [`OperationsOnTheStreamsLoadedInThisRuntime`] for a stub
/// `RuntimeOperations` type as a runtime that loads one stream, the stub:
/// every call naming it, or naming none, reaches a clone of the stub (whose
/// state is shared behind `Arc`s); the machine's shutdown request records its
/// reason on a `recorded_shutdown_reasons` field; the exchange answers from an
/// `exchange` [`StubSurfaceExchange`] field; the node catalog is the native
/// registry's; the stub stream's log holds no record; it lists as attached,
/// and every stream action is refused.
///
/// [`OperationsOnTheStreamsLoadedInThisRuntime`]: ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime
macro_rules! a_stub_runtime_loading_this_stub_as_its_only_stream {
    ($stub_type:ty) => {
        impl ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime for $stub_type {
            fn runtime_operations_of_the_stream_a_call_names(
                &self,
                stream_name: Option<&str>,
            ) -> ::streamlib::sdk::error::Result<
                ::std::sync::Arc<dyn ::streamlib::sdk::runtime::RuntimeOperations>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_name,
                )?;
                Ok(::std::sync::Arc::new(self.clone()))
            }
            fn node_catalog_of_the_stream_a_call_names(
                &self,
                stream_name: Option<&str>,
            ) -> ::streamlib::sdk::error::Result<
                Vec<::streamlib::sdk::descriptors::ProcessorDescriptor>,
            > {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_name,
                )?;
                Ok(::streamlib::sdk::processors::PROCESSOR_REGISTRY.list_registered())
            }
            fn event_topic_of_the_stream_a_call_names(
                &self,
                stream_name: Option<&str>,
            ) -> ::streamlib::sdk::error::Result<String> {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    stream_name,
                )?;
                Ok($crate::control_plane_stub_support::STUB_STREAM_EVENT_TOPIC.to_string())
            }
            fn names_of_the_loaded_streams(&self) -> Vec<String> {
                vec![$crate::control_plane_stub_support::STUB_STREAM_NAME.to_string()]
            }
            fn log_records_of_the_stream_a_call_names(
                &self,
                stream_name: &str,
                after: u64,
                _max_count: usize,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::logging::LoadedStreamLogRecordsPage>
            {
                $crate::control_plane_stub_support::refuse_a_stream_the_stub_runtime_does_not_load(
                    Some(stream_name),
                )?;
                Ok(::streamlib::sdk::logging::LoadedStreamLogRecordsPage {
                    records: Vec::new(),
                    next_after: after,
                    records_no_longer_held: 0,
                })
            }
            fn run_stream(
                &self,
                _request: ::streamlib::sdk::runtime::RunStreamRequest,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamRunOutcome> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "run_stream",
                ))
            }
            fn stop_stream(
                &self,
                _stream_name: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamStopOutcome> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "stop_stream",
                ))
            }
            fn start_stream(
                &self,
                _stream_name: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamStartOutcome> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "start_stream",
                ))
            }
            fn remove_stream(
                &self,
                _stream_name: &str,
            ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::runtime::StreamRemoveOutcome> {
                Err($crate::control_plane_stub_support::the_stub_runtime_takes_no_stream_action(
                    "remove_stream",
                ))
            }
            fn list_streams(&self) -> Vec<::streamlib::sdk::runtime::StreamListing> {
                vec![::streamlib::sdk::runtime::StreamListing {
                    name: $crate::control_plane_stub_support::STUB_STREAM_NAME.to_string(),
                    state: ::streamlib::sdk::runtime::StreamListingState::Attached,
                    project_directory: ::std::path::PathBuf::new(),
                    node_count: None,
                }]
            }
            fn expose_port(
                &self,
                _stream_name: &str,
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
            fn unload_the_attached_stream_if_still_the_same(
                &self,
                _stream_name: &str,
                _stream_tag: ::streamlib::sdk::runtime::LoadedStreamTag,
            ) -> bool {
                false
            }
            fn request_the_shutdown_of_every_loaded_stream(
                &self,
                reason: &str,
            ) -> ::streamlib::sdk::error::Result<()> {
                self.recorded_shutdown_reasons
                    .lock()
                    .push(reason.to_string());
                Ok(())
            }
            fn exchange_published_surface_id_for_png_image_bytes_async(
                &self,
                published_surface_id: String,
                downscale_long_edge_pixel_cap: Option<u32>,
            ) -> ::streamlib::sdk::runtime::BoxFuture<
                '_,
                ::streamlib::sdk::error::Result<
                    ::streamlib::sdk::runtime::ExchangedPublishedSurfaceFramePngImage,
                >,
            > {
                let exchange = self.exchange.clone();
                Box::pin(async move {
                    exchange.answer_for(&published_surface_id, downscale_long_edge_pixel_cap)
                })
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
/// never blocks a worker, and it asks for every stream's shutdown.
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
            Box::pin(async move {
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
        fn this_runtimes_name(&self) -> &str {
            $crate::control_plane_stub_support::STUB_RUNTIME_NAME
        }
        fn request_this_streams_shutdown(
            &self,
            _reason: &str,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!(
                "the control plane asks for the shutdown of every loaded stream, never of one"
            )
        }
    };
}

pub(crate) use graph_mutation_ops_record_the_call;

/// A published pool frame id is `<slot>#<generation>`, and `#` starts a URL
/// fragment — so the wire form is percent-encoded and a front end has to
/// hand the operation the decoded id.
pub(crate) const STUB_EXCHANGED_FRAME_SURFACE_ID: &str = "pool-slot-a#7";

/// [`STUB_EXCHANGED_FRAME_SURFACE_ID`] as it travels in a URL path segment.
pub(crate) const STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED: &str = "pool-slot-a%237";

/// The `(surface id, downscale cap)` pairs a front end handed the exchange
/// operation, in call order.
pub(crate) type RecordedSurfaceExchangeCalls =
    ::std::sync::Arc<::parking_lot::Mutex<Vec<(String, Option<u32>)>>>;

/// What the stub's exchange answers, shared by the route tests and the MCP
/// tool tests: both owe the operation the same arguments and owe their
/// caller its bytes unaltered, so both assert against one recorder.
///
/// The bytes are deliberately not a PNG. What a front end owes is
/// pass-through — verbatim for REST, base64 for MCP — and a recognizable
/// string proves that where a real image would only prove the encoder
/// still works.
#[derive(Clone, Default)]
pub(crate) struct StubSurfaceExchange {
    pub(crate) recorded_calls: RecordedSurfaceExchangeCalls,
    pub(crate) recycled_surface_id: Option<String>,
}

/// The stub's answer bytes: recognizable, and not a PNG.
pub(crate) const STUB_EXCHANGED_IMAGE_BYTES: &[u8] = b"stub-exchanged-image-bytes";

/// The extent the stub's surface itself carries — the *true* extent a
/// downscaled answer must still report.
pub(crate) const STUB_SOURCE_SURFACE_EXTENT: (u32, u32) = (1920, 1080);

impl StubSurfaceExchange {
    /// A stub that refuses `recycled_surface_id` as a recycled frame and
    /// answers every other id.
    pub(crate) fn refusing_as_recycled(recycled_surface_id: &str) -> Self {
        Self {
            recycled_surface_id: Some(recycled_surface_id.to_string()),
            ..Self::default()
        }
    }

    /// Record the call and answer it, modelling a cap that bounds the long
    /// edge with the short edge taking its ratio.
    pub(crate) fn answer_for(
        &self,
        published_surface_id: &str,
        downscale_long_edge_pixel_cap: Option<u32>,
    ) -> ::streamlib::sdk::error::Result<
        ::streamlib::sdk::runtime::ExchangedPublishedSurfaceFramePngImage,
    > {
        self.recorded_calls.lock().push((
            published_surface_id.to_string(),
            downscale_long_edge_pixel_cap,
        ));
        if self.recycled_surface_id.as_deref() == Some(published_surface_id) {
            return Err(::streamlib::sdk::error::Error::SurfaceFrameRecycled {
                surface_id: published_surface_id.to_string(),
                published_generation: 7,
                current_generation: 9,
            });
        }
        let (source_surface_pixel_width, source_surface_pixel_height) = STUB_SOURCE_SURFACE_EXTENT;
        let (encoded_image_pixel_width, encoded_image_pixel_height) =
            match downscale_long_edge_pixel_cap {
                Some(cap) if cap < source_surface_pixel_width => (
                    cap,
                    source_surface_pixel_height * cap / source_surface_pixel_width,
                ),
                _ => (source_surface_pixel_width, source_surface_pixel_height),
            };
        Ok(
            ::streamlib::sdk::runtime::ExchangedPublishedSurfaceFramePngImage {
                png_image_bytes: STUB_EXCHANGED_IMAGE_BYTES.to_vec(),
                encoded_image_pixel_width,
                encoded_image_pixel_height,
                source_surface_pixel_width,
                source_surface_pixel_height,
            },
        )
    }
}

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
    pub(crate) fn over(
        operations_on_the_loaded_streams: ::std::sync::Arc<
            dyn ::streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime,
        >,
    ) -> Self {
        let socket_directory = ::tempfile::tempdir().expect("a temp directory");
        let local_api_socket_path = socket_directory.path().join("local-api-Rtest.sock");
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
