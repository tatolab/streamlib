// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Continuous host processor for Python-defined processors.
//!
//! Use this for Python processors that run in a continuous loop.
//! The Python class must implement `process(self, ctx)`.

use std::sync::Arc;

use streamlib::{ContinuousProcessor, LinkInput, LinkOutput, Result, RuntimeContext, VideoFrame};

use crate::python_processor_core::{PythonProcessorConfig, PythonProcessorCore};

/// Continuous host processor for Python-defined processors.
///
/// This processor loads a Python project and executes it via PyO3.
/// The Python class must be decorated with `@processor(execution="Continuous")`
/// and implement `process(self, ctx)`.
///
/// Use this for generators, sources, polling, or batch processing where
/// `process()` is called repeatedly in a loop.
#[streamlib::processor(
    execution = Continuous,
    description = "Continuous host processor for Python-defined processors",
    display_name_from_config = "class_name"
)]
pub struct PythonContinuousHostProcessor {
    #[streamlib::config]
    config: PythonProcessorConfig,

    #[streamlib::input(description = "Video input")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Video output")]
    video_out: Arc<LinkOutput<VideoFrame>>,

    /// Shared Python processor core.
    #[allow(dead_code)]
    core: PythonProcessorCore,
}

impl PythonContinuousHostProcessor::Processor {
    fn core(&self) -> &PythonProcessorCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut PythonProcessorCore {
        &mut self.core
    }
}

impl ContinuousProcessor for PythonContinuousHostProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.core_mut().setup_common(&ctx.gpu)?;
        self.core_mut().init_python_context()?;

        // Validate execution mode
        if let Some(ref metadata) = self.core().metadata {
            if metadata.execution != "Continuous" {
                tracing::warn!(
                    "PythonContinuousHostProcessor: Python processor '{}' declares execution='{}' but is being run as Continuous",
                    metadata.name,
                    metadata.execution
                );
            }
        }

        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        self.core_mut().teardown_common()
    }

    async fn on_pause(&mut self) -> Result<()> {
        self.core().call_python_on_pause()
    }

    async fn on_resume(&mut self) -> Result<()> {
        self.core().call_python_on_resume()
    }

    fn process(&mut self) -> Result<()> {
        // For continuous mode, we may or may not have input
        // Try to read input if available
        let input_frame = self.video_in.read();

        // Call Python process()
        if let Some(output_frame) = self.core().call_python_process(input_frame.clone())? {
            self.video_out.write(output_frame);
        }

        Ok(())
    }
}
