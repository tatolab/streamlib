// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Continuous processor for Python-defined processors.
//!
//! Use this for Python processors that run in a continuous loop.
//! The Python class must implement `process(self, ctx)`.

use std::sync::Arc;

use streamlib::{ContinuousProcessor, LinkInput, LinkOutput, Result, RuntimeContext, VideoFrame};

use crate::python_core::PythonCore;
use crate::python_processor_core::PythonProcessorConfig;

/// Continuous processor for Python-defined processors.
///
/// This processor runs Python code in an isolated subprocess, enabling:
/// - True dependency isolation (different numpy versions in different processors)
/// - Crash isolation (subprocess crash doesn't take down runtime)
/// - Own GIL per subprocess (true parallelism for Python)
/// - Zero-copy GPU frame sharing via RHI external handles
///
/// The Python class must be decorated with `@processor(execution="Continuous")`
/// and implement `process(self, ctx)`.
#[streamlib::processor(
    execution = Continuous,
    execution_interval_ms = 16,
    description = "Continuous processor for Python-defined processors",
    display_name_from_config = "class_name"
)]
pub struct PythonContinuousProcessor {
    #[streamlib::config]
    config: PythonProcessorConfig,

    #[streamlib::input(description = "Video input")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Video output")]
    video_out: Arc<LinkOutput<VideoFrame>>,

    /// Core handling subprocess IPC and frame bridging.
    #[allow(dead_code)]
    core: PythonCore,
}

impl PythonContinuousProcessor::Processor {
    fn core_mut(&mut self) -> &mut PythonCore {
        &mut self.core
    }
}

impl ContinuousProcessor for PythonContinuousProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        let config = self.config.clone();
        let runtime_id = ctx.runtime_id();
        let processor_id = ctx.processor_id().ok_or_else(|| {
            streamlib::StreamError::Runtime("processor_id not set on RuntimeContext".into())
        })?;

        self.core_mut()
            .setup_common(config, &runtime_id, &processor_id)?;

        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        self.core_mut().teardown_common()
    }

    async fn on_pause(&mut self) -> Result<()> {
        self.core_mut().on_pause()
    }

    async fn on_resume(&mut self) -> Result<()> {
        self.core_mut().on_resume()
    }

    fn process(&mut self) -> Result<()> {
        // For continuous mode, we may or may not have input
        // Try to read input if available
        let input_frame = self.video_in.read();

        // Send input frame to subprocess (if available)
        if let Some(ref frame) = input_frame {
            self.core_mut().send_input_frame("video_in", frame)?;
        }

        // Trigger process cycle in subprocess
        self.core_mut().process()?;

        // Receive output frame from subprocess
        if let Some(output_frame) = self.core_mut().recv_output_frame("video_out")? {
            self.video_out.write(output_frame);
        }

        Ok(())
    }
}
