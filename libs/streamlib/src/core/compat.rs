//! Backwards compatibility aliases.
//!
//! These will be removed in the next major version.

#[deprecated(since = "0.2.0", note = "Use ExecutionConfig")]
pub type SchedulingConfig = super::execution::ExecutionConfig;

#[deprecated(since = "0.2.0", note = "Use ProcessExecution")]
pub type SchedulingMode = super::execution::ProcessExecution;
