// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test scaffolding for the two control-plane front ends.

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
    };
}

pub(crate) use graph_mutation_ops_are_unreachable;

/// A published pool frame id is `<slot>#<generation>`, and `#` starts a URL
/// fragment — so the wire form is percent-encoded and a front end has to
/// hand the operation the decoded id.
pub(crate) const EXCHANGED_FRAME_ID: &str = "pool-slot-a#7";

/// [`EXCHANGED_FRAME_ID`] as it travels in a URL path segment.
pub(crate) const EXCHANGED_FRAME_ID_PERCENT_ENCODED: &str = "pool-slot-a%237";

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
