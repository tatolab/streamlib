// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reactive host processor for Python-defined processors.
//!
//! Use this for Python processors that react to input data.
//! The Python class must implement `process(self, ctx)`.

use std::sync::Arc;

use streamlib::{LinkInput, LinkOutput, ReactiveProcessor, Result, RuntimeContext, VideoFrame};

use crate::python_processor_core::{PythonProcessorConfig, PythonProcessorCore};

// Re-export config for backward compatibility
pub use crate::python_processor_core::PythonProcessorConfig as PythonHostProcessorConfig;

/// Reactive host processor for Python-defined processors.
///
/// This processor loads a Python project and executes it via PyO3.
/// The Python class must be decorated with `@processor(execution="Reactive")`
/// and implement `process(self, ctx)`.
///
/// Use this when Python processor reacts to input data - `process()` is called
/// each time data arrives on an input port.
#[streamlib::processor(
    execution = Reactive,
    description = "Reactive host processor for Python-defined processors",
    display_name_from_config = "class_name"
)]
pub struct PythonReactiveHostProcessor {
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

impl PythonReactiveHostProcessor::Processor {
    fn core(&self) -> &PythonProcessorCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut PythonProcessorCore {
        &mut self.core
    }
}

impl ReactiveProcessor for PythonReactiveHostProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        let config = self.config.clone();
        self.core_mut().setup_common(config, &ctx.gpu)?;
        self.core_mut().init_python_context()?;

        // Validate execution mode
        if let Some(ref metadata) = self.core().metadata {
            if metadata.execution != "Reactive" {
                tracing::warn!(
                    "PythonReactiveHostProcessor: Python processor '{}' declares execution='{}' but is being run as Reactive",
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
        // Read input frame
        let input_frame = match self.video_in.read() {
            Some(frame) => frame,
            None => return Ok(()),
        };

        // Call Python process()
        if let Some(output_frame) = self.core().call_python_process(Some(input_frame.clone()))? {
            self.video_out.write(output_frame);
        }

        Ok(())
    }
}

/// Backward compatibility: Re-export PythonReactiveHostProcessor as PythonHostProcessor.
#[allow(non_snake_case)]
pub mod PythonHostProcessor {
    pub use super::PythonReactiveHostProcessor::*;
}
