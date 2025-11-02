//! Runtime contexts for processor initialization
//!
//! Following GStreamer's GstContext pattern, contexts provide access to shared
//! runtime resources that processors need during initialization and execution.
//!
//! ## Context Hierarchy
//!
//! - **GpuContext**: Shared WebGPU device and queue for zero-copy texture sharing
//! - **RuntimeContext**: Top-level context containing GPU context and future resources
//!
//! ## Usage Pattern
//!
//! ```rust,ignore
//! // Runtime creates context during initialization
//! let gpu_ctx = GpuContext::init_for_platform().await?;
//! let runtime_ctx = RuntimeContext::new(gpu_ctx);
//!
//! // Processors receive context during start()
//! impl StreamElement for MyProcessor {
//!     fn start(&mut self, ctx: &RuntimeContext) -> Result<()> {
//!         // Store GPU context for later use
//!         self.device = ctx.gpu.device().clone();
//!         Ok(())
//!     }
//! }
//! ```

mod gpu_context;
mod runtime_context;

pub use gpu_context::GpuContext;
pub use runtime_context::RuntimeContext;
