// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test scaffolding for the control plane's two front ends and for
//! the tests that assert on what it logs.

/// Implement every graph-mutating [`RuntimeOperations`] method as `unreachable!`,
/// naming `$surface` (`"route"` / `"tool"`) in each panic.
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
            ::streamlib::sdk::error::Result<::streamlib::sdk::graph::ProcessorUniqueId>,
        > {
            unreachable!(concat!(
                "the control plane serves no processor-creation ",
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
        fn this_runtimes_name_on_the_mesh(&self) -> String {
            $crate::control_plane_stub_support::STUB_RUNTIME_NAME.to_string()
        }
        fn request_link_on_remote_input_runtime(
            &self,
            _from: ::streamlib::sdk::graph::OutputLinkPortRef,
            _to: ::streamlib::sdk::graph::MeshPortAddress,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkRequestUniqueId> {
            unreachable!(concat!("the control plane serves no connect ", $surface))
        }
        fn request_disconnect_on_remote_input_runtime(
            &self,
            _input_runtime_name: String,
            _link_id: ::streamlib::sdk::graph::LinkUniqueId,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkRequestUniqueId> {
            unreachable!(concat!("the control plane serves no disconnect ", $surface))
        }
        fn cancel_link_request(
            &self,
            _link_request_id: &::streamlib::sdk::graph::LinkRequestUniqueId,
        ) -> ::streamlib::sdk::error::Result<()> {
            unreachable!(concat!("the control plane serves no disconnect ", $surface))
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
    /// A link whose input is on another runtime, asked for rather than applied.
    RequestLinkOnRemoteInputRuntime(
        ::streamlib::sdk::graph::OutputLinkPortRef,
        ::streamlib::sdk::graph::MeshPortAddress,
    ),
    /// A link on another runtime, asked to go.
    RequestDisconnectOnRemoteInputRuntime(String, ::streamlib::sdk::graph::LinkUniqueId),
    /// A request cancelled before any runtime applied it.
    CancelLinkRequest(::streamlib::sdk::graph::LinkRequestUniqueId),
}

/// The graph mutations a front end handed the runtime, in call order.
pub(crate) type RecordedGraphMutations =
    ::std::sync::Arc<::parking_lot::Mutex<Vec<RecordedGraphMutation>>>;

/// The id the stub answers every `add_processor` with.
pub(crate) const STUB_ADDED_PROCESSOR_ID: &str = "stub-added-processor";

/// The id the stub answers every `connect` with.
pub(crate) const STUB_CREATED_LINK_ID: &str = "stub-created-link";

/// The id the stub answers every link request with.
pub(crate) const STUB_MADE_LINK_REQUEST_ID: &str = "stub-made-link-request";

/// The mesh name the stub runtime answers to, which is what a front end
/// reports as the runtime a link's input is on.
pub(crate) const STUB_RUNTIME_NAME: &str = "stub-runtime";

/// What a stub runtime answers an `add_processor` with when the test has armed
/// a refusal, standing in for an engine-side one — a display name that cannot
/// be a mesh address chunk, an unknown class.
pub(crate) type ArmedAddProcessorRefusal = ::std::sync::Arc<::parking_lot::Mutex<Option<String>>>;

/// Implement the four async graph-mutating [`RuntimeOperations`] methods by
/// recording the call on a `recorded_graph_mutations` field and answering a
/// fixed id, so a front-end test can assert its tool reached the matching op
/// with the arguments the caller sent. An `armed_add_processor_refusal` the
/// test filled makes the add refuse instead, so a front end's handling of an
/// engine refusal is testable without an engine. The blocking wrappers stay
/// unreachable: a front end awaits, it never blocks a worker.
macro_rules! graph_mutation_ops_record_the_call {
    () => {
        fn add_processor_async(
            &self,
            spec: ::streamlib::sdk::processors::ProcessorSpec,
        ) -> ::streamlib::sdk::runtime::BoxFuture<
            '_,
            ::streamlib::sdk::error::Result<::streamlib::sdk::graph::ProcessorUniqueId>,
        > {
            self.recorded_graph_mutations.lock().push(
                $crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec),
            );
            let armed_refusal = self.armed_add_processor_refusal.lock().clone();
            Box::pin(async move {
                match armed_refusal {
                    Some(refusal) => Err(::streamlib::sdk::error::Error::Configuration(refusal)),
                    None => Ok(::streamlib::sdk::graph::ProcessorUniqueId::from(
                        $crate::control_plane_stub_support::STUB_ADDED_PROCESSOR_ID,
                    )),
                }
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
        fn this_runtimes_name_on_the_mesh(&self) -> String {
            $crate::control_plane_stub_support::STUB_RUNTIME_NAME.to_string()
        }
        fn request_link_on_remote_input_runtime(
            &self,
            from: ::streamlib::sdk::graph::OutputLinkPortRef,
            to: ::streamlib::sdk::graph::MeshPortAddress,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkRequestUniqueId> {
            self.recorded_graph_mutations.lock().push(
                $crate::control_plane_stub_support::RecordedGraphMutation::
                    RequestLinkOnRemoteInputRuntime(from, to),
            );
            Ok(::streamlib::sdk::graph::LinkRequestUniqueId::from(
                $crate::control_plane_stub_support::STUB_MADE_LINK_REQUEST_ID,
            ))
        }
        fn request_disconnect_on_remote_input_runtime(
            &self,
            input_runtime_name: String,
            link_id: ::streamlib::sdk::graph::LinkUniqueId,
        ) -> ::streamlib::sdk::error::Result<::streamlib::sdk::graph::LinkRequestUniqueId> {
            self.recorded_graph_mutations.lock().push(
                $crate::control_plane_stub_support::RecordedGraphMutation::
                    RequestDisconnectOnRemoteInputRuntime(input_runtime_name, link_id),
            );
            Ok(::streamlib::sdk::graph::LinkRequestUniqueId::from(
                $crate::control_plane_stub_support::STUB_MADE_LINK_REQUEST_ID,
            ))
        }
        fn cancel_link_request(
            &self,
            link_request_id: &::streamlib::sdk::graph::LinkRequestUniqueId,
        ) -> ::streamlib::sdk::error::Result<()> {
            self.recorded_graph_mutations.lock().push(
                $crate::control_plane_stub_support::RecordedGraphMutation::CancelLinkRequest(
                    link_request_id.clone(),
                ),
            );
            Ok(())
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

/// Implement the exchange operation over a [`StubSurfaceExchange`] field
/// named `exchange`, so both front ends' stubs answer it identically.
macro_rules! surface_exchange_op_answers_the_stub {
    () => {
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
    };
}

pub(crate) use surface_exchange_op_answers_the_stub;

/// One tracing span or event raised while [`CapturedTracingRecords`] was
/// installed. A span carries its name as the message; it has no other.
pub(crate) struct CapturedTracingRecord {
    pub(crate) level: ::tracing::Level,
    pub(crate) target: String,
    pub(crate) message: String,
}

/// Every tracing span and event raised on the calling thread while this is its
/// default subscriber's layer.
///
/// Spans are captured alongside events because the control plane's request
/// trace levels a span that raises no event of its own.
#[derive(Clone, Default)]
pub(crate) struct CapturedTracingRecords(
    ::std::sync::Arc<::parking_lot::Mutex<Vec<::std::sync::Arc<CapturedTracingRecord>>>>,
);

impl CapturedTracingRecords {
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
    ) -> Vec<::std::sync::Arc<CapturedTracingRecord>> {
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

impl<S: ::tracing::Subscriber> ::tracing_subscriber::layer::Layer<S> for CapturedTracingRecords {
    fn on_new_span(
        &self,
        span: &::tracing::span::Attributes<'_>,
        _id: &::tracing::span::Id,
        _context: ::tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = span.metadata();
        self.0
            .lock()
            .push(::std::sync::Arc::new(CapturedTracingRecord {
                level: *metadata.level(),
                target: metadata.target().to_string(),
                message: metadata.name().to_string(),
            }));
    }

    fn on_event(
        &self,
        event: &::tracing::Event<'_>,
        _context: ::tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut message = String::new();
        event.record(&mut OneCapturedRecordMessage(&mut message));
        let metadata = event.metadata();
        self.0
            .lock()
            .push(::std::sync::Arc::new(CapturedTracingRecord {
                level: *metadata.level(),
                target: metadata.target().to_string(),
                message,
            }));
    }
}

/// Renders an event's `message` field, the one field a log assertion reads.
struct OneCapturedRecordMessage<'a>(&'a mut String);

impl ::tracing::field::Visit for OneCapturedRecordMessage<'_> {
    fn record_debug(&mut self, field: &::tracing::field::Field, value: &dyn ::std::fmt::Debug) {
        use ::std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        }
    }
}
