// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use tracing::Instrument as _;

use super::operations::{BoxFuture, RuntimeOperations};
use super::surface_image_exchange::exchange_published_surface_id_for_png_image_bytes;
use super::{
    ExchangedPublishedSurfaceFramePngImage, LoadedStreamInThisRuntime, LoadedStreamTag,
    OutputPortExposureOutcome, RunStreamRequest, Runner, StreamListing, StreamRemoveOutcome,
    StreamRunOutcome, StreamStartOutcome, StreamStopOutcome,
};
use crate::core::ProcessorDescriptor;
use crate::core::error::{Error, Result};
use crate::core::graph::OutputPortExposureLevel;
use crate::core::logging::LoadedStreamLogRecordsPage;
use crate::core::pubsub::topics;

/// What the local API reaches a runtime through: the stream a call names, a
/// stream's node catalog and log records, the stream actions, the machine's
/// shutdown request and the surface exchange.
///
/// Every stream action blocks — a load runs a compile and describes — so the
/// local API calls each from a blocking task.
///
/// Implemented by [`Runner`], and by the control plane's test stubs.
pub trait OperationsOnTheStreamsLoadedInThisRuntime: Send + Sync {
    /// The operations on the stream `stream_name` names; `None` names the only
    /// loaded stream. Refused with [`Error::NotFound`] naming the loaded
    /// streams when the named one is not loaded, and when `None` meets none or
    /// several.
    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_name: Option<&str>,
    ) -> Result<Arc<dyn RuntimeOperations>>;

    /// The node types the stream `stream_name` names resolves — the native
    /// ones, then the ones described in its own interpreter — with the same
    /// `None` rule and the same [`Error::NotFound`] refusal.
    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_name: Option<&str>,
    ) -> Result<Vec<ProcessorDescriptor>>;

    /// The topic the events of the stream `stream_name` names publish on,
    /// with the same `None` rule.
    fn event_topic_of_the_stream_a_call_names(&self, stream_name: Option<&str>) -> Result<String>;

    /// The cast names of the loaded streams, in order.
    fn names_of_the_loaded_streams(&self) -> Vec<String>;

    /// The log records of the loaded stream `stream_name` names, numbered
    /// after `after`, at most `max_count` of them; refused naming the loaded
    /// streams when it is not loaded.
    fn log_records_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
        after: u64,
        max_count: usize,
    ) -> Result<LoadedStreamLogRecordsPage>;

    /// [`Runner::run_stream`].
    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome>;

    /// [`Runner::stop_stream`].
    fn stop_stream(&self, stream_name: &str) -> Result<StreamStopOutcome>;

    /// [`Runner::start_stream`].
    fn start_stream(&self, stream_name: &str) -> Result<StreamStartOutcome>;

    /// [`Runner::remove_stream`].
    fn remove_stream(&self, stream_name: &str) -> Result<StreamRemoveOutcome>;

    /// [`Runner::list_streams`].
    fn list_streams(&self) -> Vec<StreamListing>;

    /// [`Runner::expose_port`].
    fn expose_port(
        &self,
        stream_name: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome>;

    /// [`Runner::unload_the_attached_stream_if_still_the_same`].
    fn unload_the_attached_stream_if_still_the_same(
        &self,
        stream_name: &str,
        stream_tag: LoadedStreamTag,
    ) -> bool;

    /// Ask for the shutdown of every loaded stream, with a human-readable
    /// `reason` logged for attribution. Fire-and-forget; never blocks.
    fn request_the_shutdown_of_every_loaded_stream(&self, reason: &str) -> Result<()>;

    /// Exchange a published surface id for that frame's pixels, encoded as a
    /// PNG.
    ///
    /// `published_surface_id` is a surface id a bag carried
    /// (`<slot>#<generation>` for a pooled frame); a retired one fails with
    /// [`Error::SurfaceFrameRecycled`] before any bytes move. A surface id is
    /// unique on the machine, so the exchange names no stream; it needs a
    /// started stream, whose GPU context is the engine's.
    /// `downscale_long_edge_pixel_cap` bounds the encoded image's long edge,
    /// preserving aspect and never upscaling. Why the verb has this shape:
    /// `docs/decisions/control-plane-pixel-exchange.md`.
    fn exchange_published_surface_id_for_png_image_bytes_async(
        &self,
        published_surface_id: String,
        downscale_long_edge_pixel_cap: Option<u32>,
    ) -> BoxFuture<'_, Result<ExchangedPublishedSurfaceFramePngImage>>;
}

impl Runner {
    /// The stream `stream_name` names; `None` names the only loaded stream.
    pub fn the_stream_a_call_names(
        &self,
        stream_name: Option<&str>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        if let Some(stream_name) = stream_name {
            return self.loaded_stream_named(stream_name);
        }
        match self.every_loaded_stream().as_slice() {
            [only] => Ok(Arc::clone(only)),
            [] => Err(Error::NotFound(
                "no stream is loaded in this runtime, so there is none to name".to_string(),
            )),
            several => Err(Error::NotFound(format!(
                "{} streams are loaded in this runtime, so a call has to name one with `stream`. \
                 Loaded: {}",
                several.len(),
                several
                    .iter()
                    .map(|stream| stream.stream_name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

impl OperationsOnTheStreamsLoadedInThisRuntime for Runner {
    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_name: Option<&str>,
    ) -> Result<Arc<dyn RuntimeOperations>> {
        self.the_stream_a_call_names(stream_name)
            .map(|stream| stream as Arc<dyn RuntimeOperations>)
    }

    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_name: Option<&str>,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.the_stream_a_call_names(stream_name)
            .map(|stream| stream.node_types_this_stream_resolves().node_catalog())
    }

    fn event_topic_of_the_stream_a_call_names(&self, stream_name: Option<&str>) -> Result<String> {
        self.the_stream_a_call_names(stream_name)
            .map(|stream| topics::loaded_stream(stream.loaded_stream_identity()))
    }

    fn names_of_the_loaded_streams(&self) -> Vec<String> {
        Runner::names_of_the_loaded_streams(self)
    }

    fn log_records_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
        after: u64,
        max_count: usize,
    ) -> Result<LoadedStreamLogRecordsPage> {
        self.loaded_stream_named(stream_name)
            .map(|stream| stream.log_records_after(after, max_count))
    }

    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        Runner::run_stream(self, request)
    }

    fn stop_stream(&self, stream_name: &str) -> Result<StreamStopOutcome> {
        Runner::stop_stream(self, stream_name)
    }

    fn start_stream(&self, stream_name: &str) -> Result<StreamStartOutcome> {
        Runner::start_stream(self, stream_name)
    }

    fn remove_stream(&self, stream_name: &str) -> Result<StreamRemoveOutcome> {
        Runner::remove_stream(self, stream_name)
    }

    fn list_streams(&self) -> Vec<StreamListing> {
        Runner::list_streams(self)
    }

    fn expose_port(
        &self,
        stream_name: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        Runner::expose_port(self, stream_name, node, port, level)
    }

    fn unload_the_attached_stream_if_still_the_same(
        &self,
        stream_name: &str,
        stream_tag: LoadedStreamTag,
    ) -> bool {
        Runner::unload_the_attached_stream_if_still_the_same(self, stream_name, stream_tag)
    }

    fn request_the_shutdown_of_every_loaded_stream(&self, reason: &str) -> Result<()> {
        Runner::request_the_shutdown_of_every_loaded_stream(self, reason)
    }

    fn exchange_published_surface_id_for_png_image_bytes_async(
        &self,
        published_surface_id: String,
        downscale_long_edge_pixel_cap: Option<u32>,
    ) -> BoxFuture<'_, Result<ExchangedPublishedSurfaceFramePngImage>> {
        // Built here and entered by the async block: an instrumented
        // `-> BoxFuture` fn opens and closes its span while the future is
        // built, covering none of the copy or the encode.
        let exchange_span = tracing::info_span!(
            "runtime.exchange",
            surface_id = %published_surface_id,
            downscale_long_edge_pixel_cap = ?downscale_long_edge_pixel_cap,
        );

        // Read before spawning: with no started stream there is no pool to
        // claim from and no device to convert on.
        let gpu_context = self
            .every_loaded_stream()
            .iter()
            .find_map(|stream| stream.runtime_context_while_started())
            .map(|runtime_context| runtime_context.gpu.clone())
            .ok_or_else(|| {
                Error::Runtime(
                    "no stream in this runtime has started, so no surface can be exchanged for \
                     an image; start a stream first"
                        .into(),
                )
            });

        Box::pin(
            async move {
                let gpu_context = gpu_context?;
                // The copy blocks on the GPU and the encode on the CPU; both
                // run off the async worker.
                tokio::task::spawn_blocking(move || {
                    exchange_published_surface_id_for_png_image_bytes(
                        &gpu_context,
                        &published_surface_id,
                        downscale_long_edge_pixel_cap,
                    )
                })
                .await
                .map_err(|join_error| {
                    Error::Runtime(format!(
                        "surface-exchange task failed to join: {join_error}"
                    ))
                })?
            }
            .instrument(exchange_span),
        )
    }
}
