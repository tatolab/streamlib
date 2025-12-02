pub(crate) mod execution_graph;
pub(crate) mod running;
mod simple;
mod state;
pub(crate) mod thread_runner;
mod traits;

pub use simple::{BoxedProcessor, RuntimeStatus, SimpleExecutor};
pub use state::ExecutorState;
pub use traits::{ExecutorLifecycle, GraphCompiler};

// Re-export for backwards compatibility (now in compiler module)
pub use crate::core::compiler::{compute_delta, GraphDelta};
