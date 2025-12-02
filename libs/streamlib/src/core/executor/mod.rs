pub(crate) mod delta;
pub(crate) mod execution_graph;
pub(crate) mod running;
mod simple;
mod state;
pub(crate) mod thread_runner;
mod traits;

pub use delta::{
    compute_delta, compute_delta_with_config, GraphDelta, LinkConfigChange, ProcessorConfigChange,
};
pub use simple::{BoxedProcessor, RuntimeStatus, SimpleExecutor};
pub use state::ExecutorState;
pub use traits::{ExecutorLifecycle, GraphCompiler};
