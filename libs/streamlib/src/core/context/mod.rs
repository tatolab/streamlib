//! Runtime contexts for processor initialization
//!
//! Following GStreamer's GstContext pattern, contexts provide access to shared
//! runtime resources that processors need during initialization and execution.
//!
//! ## Context Hierarchy
//!
//! - **GpuContext**: Shared WebGPU device and queue for zero-copy texture sharing
//! - **AudioContext**: System-wide audio configuration (sample rate, buffer size)
//! - **RuntimeContext**: Top-level context containing GPU and audio contexts
//!
//! ## Usage Pattern
//!
//! ```rust,ignore
//! // Runtime creates context during initialization
//! let gpu_ctx = GpuContext::init_for_platform().await?;
//! let audio_ctx = AudioContext::new(48000, 512);
//! let runtime_ctx = RuntimeContext::new(gpu_ctx)
//!     .with_audio_context(audio_ctx);
//!
//! // Processors receive context during start()
//! impl StreamElement for MyProcessor {
//!     fn start(&mut self, ctx: &RuntimeContext) -> Result<()> {
//!         // Store GPU context for later use
//!         self.device = ctx.gpu.device().clone();
//!
//!         // Configure audio processing
//!         self.sample_rate = ctx.audio.sample_rate;
//!         self.buffer_size = ctx.audio.buffer_size;
//!         Ok(())
//!     }
//! }
//! ```

mod gpu_context;
mod audio_context;
mod runtime_context;

pub use gpu_context::GpuContext;
pub use audio_context::AudioContext;
pub use runtime_context::RuntimeContext;
