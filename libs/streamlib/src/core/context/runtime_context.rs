//! Runtime context passed to stream elements during initialization
//!
//! Provides access to shared runtime resources like GPU context, audio configuration,
//! and future resources (clocks, allocators, buffer pools).

use super::{GpuContext, AudioContext};
use crate::core::clocks::Clock;
use std::sync::Arc;

#[derive(Clone)]
pub struct RuntimeContext {
    pub gpu: GpuContext,
    pub audio: AudioContext,
    pub clock: Option<Arc<dyn Clock>>,
}

impl RuntimeContext {
    pub fn new(gpu: GpuContext) -> Self {
        Self {
            gpu,
            audio: AudioContext::default(),
            clock: None,
        }
    }

    pub fn with_audio_context(mut self, audio: AudioContext) -> Self {
        self.audio = audio;
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }
}
