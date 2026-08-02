// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The measured arm: a processor that runs a Python callable in-process, on the
//! engine's own dedicated processor thread, once per frame.

use std::cell::RefCell;

use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;

use crate::monotonic_clock::read_measurement_stamp_nanoseconds;
use crate::python_gil_attachment_anchor::PythonGilAttachmentAnchorForProcessorThread;
use crate::python_processor_callback_registry::resolve_python_callback_for_token;
use crate::synthetic_frame_measurement_preamble::{
    SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES, SyntheticFrameMeasurementPreamble,
};
use crate::synthetic_frame_wire_payload_mode::{
    SURFACE_REFERENCE_BODY_BYTES, SyntheticFrameWirePayloadMode, encode_synthetic_pixel_bytes,
    frame_pixel_byte_count,
};
use crate::zero_copy_numpy_frame_view::{
    NumpyFrameViewEscapeOutcome, invoke_python_callback_over_zero_copy_frame_view,
    preload_numpy_c_array_api_before_first_frame_view,
};

thread_local! {
    /// Holds this processor thread's CPython thread state open for the thread's
    /// lifetime. Thread-local rather than a struct field because the anchor is
    /// `!Send` by construction and the engine instantiates a processor on a
    /// different thread than the one that runs it.
    static PROCESSOR_THREAD_GIL_ANCHOR:
        RefCell<Option<PythonGilAttachmentAnchorForProcessorThread>> =
        const { RefCell::new(None) };
}

/// Frame geometry plus the registry token naming the callable to invoke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PythonCallbackStageConfiguration {
    pub frame_width_pixels: u32,
    pub frame_height_pixels: u32,
    pub channel_count: u32,
    /// Key into the process-global callback registry. A `Py<PyAny>` cannot ride
    /// the config because `Config` requires `Serialize + DeserializeOwned`
    /// (`core/processors/traits/config.rs:47-51`).
    pub python_callback_registration_token: String,
    /// Anchoring the thread state removes a per-frame mmap/munmap pair. Off is
    /// the control condition that measures what anchoring is worth.
    pub anchor_processor_thread_gil: bool,
    /// Must match the source's mode: it decides whether the callback's numpy
    /// view is over the wire payload or over this stage's locally resolved
    /// surface.
    #[serde(default)]
    pub wire_payload_mode: SyntheticFrameWirePayloadMode,
}

impl Default for PythonCallbackStageConfiguration {
    fn default() -> Self {
        Self {
            frame_width_pixels: 1920,
            frame_height_pixels: 1080,
            channel_count: 4,
            python_callback_registration_token: String::new(),
            anchor_processor_thread_gil: true,
            wire_payload_mode: SyntheticFrameWirePayloadMode::SurfaceReference,
        }
    }
}

impl PythonCallbackStageConfiguration {
    /// Pixel bytes for one frame of this geometry — the extent the callback's
    /// numpy view spans, wherever those pixels live.
    pub fn frame_pixel_byte_count(&self) -> usize {
        frame_pixel_byte_count(
            self.frame_width_pixels,
            self.frame_height_pixels,
            self.channel_count,
        )
    }
}

#[streamlib::sdk::processor(
    "@spike/pyembed/PythonCallbackStage",
    execution = reactive,
    config = crate::python_callback_stage_processor::PythonCallbackStageConfiguration,
    config_field = "configuration",
    input("frame_in", any, delivery_profile = "every_sample"),
    output("frame_out", any),
)]
pub struct PythonCallbackStageProcessor {
    resolved_python_callback: Option<Py<PyAny>>,
    observed_frame_view_escape_count: u64,
    /// The pixels the callback sees under
    /// [`SyntheticFrameWirePayloadMode::SurfaceReference`], standing in for what
    /// `GpuContext::resolve_pixel_buffer_by_surface_id`
    /// (`core/context/gpu_context.rs:681`) hands an in-process processor. It is
    /// process-local by construction — a surface reference on the wire means
    /// the pixels never made the hop, which is the whole point of the mode.
    /// Empty under [`SyntheticFrameWirePayloadMode::FullPixelPayload`].
    locally_resolved_surface_pixel_buffer: Vec<u8>,
}

impl PythonCallbackStageProcessor::Processor {
    fn ensure_processor_thread_gil_anchor_installed(&self) {
        if !self.configuration.anchor_processor_thread_gil {
            return;
        }
        PROCESSOR_THREAD_GIL_ANCHOR.with(|anchor_slot| {
            let mut anchor_slot = anchor_slot.borrow_mut();
            if anchor_slot.is_none() {
                *anchor_slot = Some(
                    PythonGilAttachmentAnchorForProcessorThread::attach_current_thread_and_park_gil(
                    ),
                );
            }
        });
    }
}

impl ReactiveProcessor for PythonCallbackStageProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let token = self
            .configuration
            .python_callback_registration_token
            .clone();
        let callable = Python::attach(|python| -> Result<Option<Py<PyAny>>> {
            // numpy's C API table is imported lazily on first use. Forcing it
            // here keeps that one-time cost out of the first measured frame,
            // where it would land in the p99.9 tail and be misread as a stall.
            preload_numpy_c_array_api_before_first_frame_view(python).map_err(|error| {
                Error::Runtime(format!("failed to import the numpy C array API: {error}"))
            })?;
            Ok(resolve_python_callback_for_token(python, &token))
        })?;
        self.resolved_python_callback = Some(callable.ok_or_else(|| {
            Error::Configuration(format!(
                "no Python callable registered under token `{token}` — register it before \
                 starting the graph"
            ))
        })?);

        if self.configuration.wire_payload_mode == SyntheticFrameWirePayloadMode::SurfaceReference {
            self.locally_resolved_surface_pixel_buffer =
                encode_synthetic_pixel_bytes(self.configuration.frame_pixel_byte_count());
        }
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let Some((mut frame_payload, frame_timestamp_nanoseconds)) =
            self.inputs.read_raw("frame_in")?
        else {
            return Ok(());
        };
        if frame_payload.len() <= SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES {
            return Err(Error::Link(format!(
                "frame payload of {} bytes carries nothing after the measurement preamble",
                frame_payload.len()
            )));
        }

        self.ensure_processor_thread_gil_anchor_installed();
        let callable = self
            .resolved_python_callback
            .as_ref()
            .ok_or_else(|| Error::Configuration("setup did not resolve a callable".to_string()))?;

        // Under SurfaceReference the wire carried a reference, so the callback's
        // view spans this stage's locally resolved surface; under
        // FullPixelPayload the pixels arrived in-band and the view spans them.
        // Checked against the *wire body's* width, not the local buffer's: under
        // SurfaceReference the local buffer is geometry-sized by construction, so
        // comparing it to the geometry would pass a full-pixel source paired with
        // a surface-reference stage without a word.
        let wire_body_byte_count = frame_payload.len() - SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES;
        let expected_wire_body_byte_count = match self.configuration.wire_payload_mode {
            SyntheticFrameWirePayloadMode::SurfaceReference => SURFACE_REFERENCE_BODY_BYTES,
            SyntheticFrameWirePayloadMode::FullPixelPayload => {
                self.configuration.frame_pixel_byte_count()
            }
        };
        if wire_body_byte_count != expected_wire_body_byte_count {
            return Err(Error::Link(format!(
                "stage is configured for {} and expects a {}-byte wire body, but {} bytes \
                 arrived — the source and stage disagree about the wire payload mode",
                self.configuration.wire_payload_mode.as_artifact_token(),
                expected_wire_body_byte_count,
                wire_body_byte_count
            )));
        }

        let callback_pixel_bytes: &mut [u8] = match self.configuration.wire_payload_mode {
            SyntheticFrameWirePayloadMode::SurfaceReference => {
                &mut self.locally_resolved_surface_pixel_buffer
            }
            SyntheticFrameWirePayloadMode::FullPixelPayload => {
                &mut frame_payload[SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES..]
            }
        };

        let callback_started_nanoseconds = read_measurement_stamp_nanoseconds();
        let escape_outcome = Python::attach(|python| {
            invoke_python_callback_over_zero_copy_frame_view(
                python,
                callable,
                callback_pixel_bytes,
                self.configuration.frame_height_pixels as usize,
                self.configuration.frame_width_pixels as usize,
                self.configuration.channel_count as usize,
            )
        })
        .map_err(|python_error| {
            Error::Runtime(format!("python stage callback raised: {python_error}"))
        })?;
        let callback_finished_nanoseconds = read_measurement_stamp_nanoseconds();

        if let NumpyFrameViewEscapeOutcome::RetainedByPythonWithRefcount(refcount) = escape_outcome
        {
            self.observed_frame_view_escape_count += 1;
            // Not fatal to the run, but it invalidates the zero-copy premise for
            // this frame and the retained view now aliases a buffer about to be
            // dropped — the number must reach the artifact, not just a log.
            tracing::warn!(
                refcount,
                total_escapes = self.observed_frame_view_escape_count,
                "python callback retained the frame view past its call"
            );
        }

        // Only the stage-duration field is patched. The sink derives
        // source_emit_to_sink_receive from the source's original stamp, so this
        // stage must never restamp the sequence number or the emit time.
        SyntheticFrameMeasurementPreamble::patch_stage_callback_nanoseconds_in_payload_prefix(
            &mut frame_payload,
            callback_finished_nanoseconds - callback_started_nanoseconds,
        );
        self.outputs
            .write_raw("frame_out", &frame_payload, frame_timestamp_nanoseconds)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The token is the whole callback-injection mechanism; a config that
    /// silently lost it would fail at setup with a confusing error instead of
    /// at validation.
    #[test]
    fn configuration_round_trips_the_callback_token_through_serde() {
        let configuration = PythonCallbackStageConfiguration {
            python_callback_registration_token: "cell-42-passthrough".to_string(),
            anchor_processor_thread_gil: false,
            ..PythonCallbackStageConfiguration::default()
        };
        let encoded = serde_json::to_value(&configuration).expect("serializes");
        let decoded: PythonCallbackStageConfiguration =
            serde_json::from_value(encoded).expect("deserializes");
        assert_eq!(decoded, configuration);
        assert_eq!(
            decoded.python_callback_registration_token,
            "cell-42-passthrough"
        );
        assert!(!decoded.anchor_processor_thread_gil);
    }

    /// Anchoring defaults on: the unanchored path costs a per-frame mmap/munmap
    /// pair, and a silent flip of this default would change every number the
    /// spike produces without changing any measurement code.
    #[test]
    fn gil_anchoring_is_on_by_default() {
        assert!(PythonCallbackStageConfiguration::default().anchor_processor_thread_gil);
    }
}
