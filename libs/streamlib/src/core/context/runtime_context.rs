//! Runtime context passed to stream elements during initialization
//!
//! Provides access to shared runtime resources like GPU context, audio configuration,
//! and future resources (clocks, allocators, buffer pools).

use super::{GpuContext, AudioContext};

#[derive(Clone)]
pub struct RuntimeContext {
    pub gpu: GpuContext,
    pub audio: AudioContext,
}

impl RuntimeContext {
    pub fn new(gpu: GpuContext) -> Self {
        Self {
            gpu,
            audio: AudioContext::default(),
        }
    }

    pub fn with_audio_context(mut self, audio: AudioContext) -> Self {
        self.audio = audio;
        self
    }
}
