// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Manual host processor for Python-defined processors.
//!
//! Use this for Python processors with manual timing control.
//! The Python class must implement `start(self, ctx)` and optionally `stop(self, ctx)`.

use std::sync::Arc;

use streamlib::{LinkInput, LinkOutput, ManualProcessor, Result, RuntimeContext, VideoFrame};

use crate::python_processor_core::{PythonProcessorConfig, PythonProcessorCore};

/// Manual host processor for Python-defined processors.
///
/// This processor loads a Python project and executes it via PyO3.
/// The Python class must be decorated with `@processor(execution="Manual")`
/// and implement `start(self, ctx)`. Optionally implement `stop(self, ctx)`.
///
/// Use this for hardware-driven processors (cameras, audio output, display)
/// where timing is controlled by external callbacks or hardware interrupts.
/// The runtime calls `start()` once, then you control all timing.
#[streamlib::processor(
    execution = Manual,
    description = "Manual host processor for Python-defined processors",
    display_name_from_config = "class_name"
)]
pub struct PythonManualHostProcessor {
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

impl PythonManualHostProcessor::Processor {
    fn core(&self) -> &PythonProcessorCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut PythonProcessorCore {
        &mut self.core
    }
}

impl ManualProcessor for PythonManualHostProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        let config = self.config.clone();
        self.core_mut().setup_common(config, &ctx)?;
        self.core_mut().init_python_context()?;

        // Validate execution mode
        if let Some(ref metadata) = self.core().metadata {
            if metadata.execution != "Manual" {
                tracing::warn!(
                    "PythonManualHostProcessor: Python processor '{}' declares execution='{}' but is being run as Manual",
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

    fn start(&mut self) -> Result<()> {
        self.core().call_python_start()
    }

    fn stop(&mut self) -> Result<()> {
        self.core().call_python_stop()
    }
}
