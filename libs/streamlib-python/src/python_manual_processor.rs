// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Manual processor for Python-defined processors.
//!
//! Use this for Python processors with manual timing control.
//! The Python class must implement `start(self, ctx)` and optionally `stop(self, ctx)`.

use std::sync::Arc;

use streamlib::{LinkInput, LinkOutput, ManualProcessor, Result, RuntimeContext, VideoFrame};

use crate::python_core::PythonCore;
use crate::python_processor_core::PythonProcessorConfig;

/// Manual processor for Python-defined processors.
///
/// This processor runs Python code in an isolated subprocess, enabling:
/// - True dependency isolation (different numpy versions in different processors)
/// - Crash isolation (subprocess crash doesn't take down runtime)
/// - Own GIL per subprocess (true parallelism for Python)
/// - Zero-copy GPU frame sharing via RHI external handles
///
/// The Python class must be decorated with `@processor(execution="Manual")`
/// and implement `start(self, ctx)`. Optionally implement `stop(self, ctx)`.
///
/// Use this for hardware-driven processors (cameras, audio output, display)
/// where timing is controlled by external callbacks or hardware interrupts.
/// The runtime calls `start()` once, then you control all timing.
#[streamlib::processor(
    execution = Manual,
    description = "Manual processor for Python-defined processors",
    display_name_from_config = "class_name"
)]
pub struct PythonManualProcessor {
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

impl PythonManualProcessor::Processor {
    fn core_mut(&mut self) -> &mut PythonCore {
        &mut self.core
    }
}

impl ManualProcessor for PythonManualProcessor::Processor {
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

    fn start(&mut self) -> Result<()> {
        // For manual mode, Python's start() handles its own timing.
        // We just trigger a process cycle which will call start() in the subprocess.
        // The subprocess runner handles the Manual mode differently.
        self.core_mut().process()
    }

    fn stop(&mut self) -> Result<()> {
        // Trigger stop in subprocess - handled by teardown
        Ok(())
    }
}
