use crate::core::context::RuntimeContext;
use crate::core::error::Result;
use crate::core::graph::Graph;

use super::ExecutorState;

pub trait Executor: Send {
    fn state(&self) -> ExecutorState;

    fn compile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()>;

    fn recompile(&mut self, graph: &Graph, ctx: &RuntimeContext) -> Result<()>;

    fn start(&mut self) -> Result<()>;

    fn stop(&mut self) -> Result<()>;

    fn pause(&mut self) -> Result<()>;

    fn resume(&mut self) -> Result<()>;

    fn run(&mut self) -> Result<()>;

    fn needs_recompile(&self) -> bool;
}
