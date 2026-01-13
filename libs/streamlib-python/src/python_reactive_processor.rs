// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reactive processor for Python-defined processors.
//!
//! Use this for Python processors that react to input data.
//! The Python class must implement `process(self, ctx)`.

use std::sync::Arc;

use streamlib::{LinkInput, LinkOutput, ReactiveProcessor, Result, RuntimeContext, VideoFrame};

use crate::python_core::PythonCore;
use crate::python_processor_core::PythonProcessorConfig;

/// Reactive processor for Python-defined processors.
///
/// This processor runs Python code in an isolated subprocess, enabling:
/// - True dependency isolation (different numpy versions in different processors)
/// - Crash isolation (subprocess crash doesn't take down runtime)
/// - Own GIL per subprocess (true parallelism for Python)
/// - Zero-copy GPU frame sharing via RHI external handles
///
/// The Python class must be decorated with `@processor(execution="Reactive")`
/// and implement `process(self, ctx)`.
///
/// Use this when Python processor reacts to input data - `process()` is called
/// each time data arrives on an input port.
#[streamlib::processor(
    execution = Reactive,
    description = "Reactive processor for Python-defined processors",
    display_name_from_config = "class_name"
)]
pub struct PythonReactiveProcessor {
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

impl PythonReactiveProcessor::Processor {
    fn core_mut(&mut self) -> &mut PythonCore {
        &mut self.core
    }
}

impl ReactiveProcessor for PythonReactiveProcessor::Processor {
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
        // Read input frame - reactive mode requires input
        let input_frame = match self.video_in.read() {
            Some(frame) => frame,
            None => return Ok(()),
        };

        // Send input frame to subprocess
        self.core_mut().send_input_frame("video_in", &input_frame)?;

        // Trigger process cycle in subprocess
        self.core_mut().process()?;

        // Receive output frame from subprocess
        if let Some(output_frame) = self.core_mut().recv_output_frame("video_out")? {
            self.video_out.write(output_frame);
        }

        Ok(())
    }
}
