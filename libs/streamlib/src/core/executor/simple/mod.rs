mod compile;
mod lifecycle;
mod processors;
mod wiring;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use super::execution_graph::ExecutionGraph;
use super::running::{RunningProcessor, WiredLink};
use super::ExecutorState;
use crate::core::compiler::Compiler;
use crate::core::context::RuntimeContext;
use crate::core::error::{Result, StreamError};
use crate::core::graph::{Graph, ProcessorId};
use crate::core::link_channel::{LinkChannel, LinkId};
use crate::core::processors::factory::ProcessorNodeFactory;
use crate::core::processors::{DynProcessor, ProcessorState};

/// Boxed processor trait object.
pub type BoxedProcessor = Box<dyn DynProcessor + Send>;

/// Runtime status snapshot.
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    pub running: bool,
    pub processor_count: usize,
    pub link_count: usize,
    pub processor_states: HashMap<ProcessorId, ProcessorState>,
}

/// Thread-per-processor executor with lock-free link channels.
pub struct SimpleExecutor {
    pub(super) state: ExecutorState,
    pub(super) graph: Option<Arc<RwLock<Graph>>>,
    pub(super) runtime_context: Option<Arc<RuntimeContext>>,
    pub(super) execution_graph: Option<ExecutionGraph>,
    pub(super) factory: Option<Arc<dyn ProcessorNodeFactory>>,
    pub(super) compiler: Option<Compiler>,
    pub(super) link_channel: LinkChannel,
    pub(super) next_processor_id: usize,
    pub(super) next_link_id: usize,
    pub(super) dirty: bool,
    #[cfg(target_os = "macos")]
    pub(super) is_macos_standalone: bool,
}

impl Default for SimpleExecutor {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Constructors
// ============================================================================

impl SimpleExecutor {
    pub fn new() -> Self {
        Self {
            state: ExecutorState::Idle,
            graph: None,
            runtime_context: None,
            execution_graph: None,
            factory: None,
            compiler: None,
            link_channel: LinkChannel::new(),
            next_processor_id: 0,
            next_link_id: 0,
            dirty: false,
            #[cfg(target_os = "macos")]
            is_macos_standalone: false,
        }
    }

    pub fn with_graph(graph: Arc<RwLock<Graph>>) -> Self {
        Self {
            graph: Some(graph),
            ..Self::new()
        }
    }

    pub fn with_graph_and_factory(
        graph: Arc<RwLock<Graph>>,
        factory: Arc<dyn ProcessorNodeFactory>,
    ) -> Self {
        let compiler = Compiler::new(Arc::clone(&factory));
        Self {
            graph: Some(graph),
            factory: Some(factory),
            compiler: Some(compiler),
            ..Self::new()
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

impl SimpleExecutor {
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    #[cfg(target_os = "macos")]
    pub fn needs_macos_event_loop(&self) -> bool {
        self.is_macos_standalone && self.state == ExecutorState::Running
    }

    pub fn set_runtime_context(&mut self, ctx: Arc<RuntimeContext>) {
        self.runtime_context = Some(ctx);
    }

    pub fn runtime_context(&self) -> Option<&RuntimeContext> {
        self.runtime_context.as_ref().map(|arc| arc.as_ref())
    }

    pub fn status(&self) -> RuntimeStatus {
        let (processor_count, link_count, processor_states) =
            if let Some(exec_graph) = &self.execution_graph {
                let states = exec_graph
                    .iter_processor_runtime()
                    .map(|(id, proc)| (id.clone(), *proc.state.lock()))
                    .collect();
                (
                    exec_graph.processor_count(),
                    exec_graph.link_count(),
                    states,
                )
            } else {
                (0, 0, HashMap::new())
            };

        RuntimeStatus {
            running: self.state == ExecutorState::Running,
            processor_count,
            link_count,
            processor_states,
        }
    }

    pub fn next_processor_id(&mut self) -> ProcessorId {
        let id = format!("processor_{}", self.next_processor_id);
        self.next_processor_id += 1;
        id
    }

    pub fn next_link_id(&mut self) -> LinkId {
        let id = format!("link_{}", self.next_link_id);
        self.next_link_id += 1;
        crate::core::link_channel::link_id::__private::new_unchecked(id)
    }

    pub fn remove_processor(&mut self, processor_id: &str) -> Result<()> {
        use super::GraphCompiler;

        let proc_id = processor_id.to_string();

        if let Some(exec_graph) = &self.execution_graph {
            if let Some(instance) = exec_graph.get_processor_runtime(&proc_id) {
                let current_state = *instance.state.lock();
                if current_state == ProcessorState::Running {
                    self.shutdown_processor(&proc_id)?;
                }
            }
        }

        if let Some(exec_graph) = &mut self.execution_graph {
            let link_ids: Vec<_> = exec_graph
                .iter_link_runtime()
                .filter(|(_, wired)| {
                    wired.source_processor() == proc_id || wired.dest_processor() == proc_id
                })
                .map(|(id, _)| id.clone())
                .collect();

            for link_id in link_ids {
                exec_graph.remove_link_runtime(&link_id);
            }
            exec_graph.remove_processor_runtime(&proc_id);
        }

        self.dirty = true;
        Ok(())
    }

    pub fn get_processor_links(&self, processor_id: &ProcessorId) -> Vec<LinkId> {
        let Some(exec_graph) = &self.execution_graph else {
            return Vec::new();
        };

        exec_graph
            .iter_link_runtime()
            .filter(|(_, wired)| {
                wired.source_processor() == processor_id || wired.dest_processor() == processor_id
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn set_executor_ref(executor: Arc<Mutex<SimpleExecutor>>) {
        use crate::core::pubsub::{topics, PUBSUB};

        PUBSUB.subscribe(topics::RUNTIME_GLOBAL, executor);
    }
}

// ============================================================================
// Internal accessors
// ============================================================================

#[allow(dead_code)]
impl SimpleExecutor {
    pub(crate) fn get_processor(&self, id: &ProcessorId) -> Option<&RunningProcessor> {
        self.execution_graph.as_ref()?.get_processor_runtime(id)
    }

    pub(crate) fn get_processor_mut(&mut self, id: &ProcessorId) -> Option<&mut RunningProcessor> {
        self.execution_graph.as_mut()?.get_processor_runtime_mut(id)
    }

    pub(crate) fn get_wired_link(&self, id: &LinkId) -> Option<&WiredLink> {
        self.execution_graph.as_ref()?.get_link_runtime(id)
    }
}

// ============================================================================
// Helper accessors
// ============================================================================

impl SimpleExecutor {
    pub(super) fn graph_ref(&self) -> Result<&Arc<RwLock<Graph>>> {
        self.graph
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("No graph reference set".into()))
    }

    pub(super) fn exec_graph_ref(&self) -> Result<&ExecutionGraph> {
        self.execution_graph
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))
    }

    pub(super) fn exec_graph_mut(&mut self) -> Result<&mut ExecutionGraph> {
        self.execution_graph
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Execution graph not initialized".into()))
    }

    pub(super) fn runtime_ctx(&self) -> Result<&Arc<RuntimeContext>> {
        self.runtime_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Runtime context not initialized".into()))
    }

    pub(super) fn factory_ref(&self) -> Result<&Arc<dyn ProcessorNodeFactory>> {
        self.factory
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("No processor factory set".into()))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::executor::ExecutorLifecycle;
    use crate::core::graph::Link;
    use crate::core::link_channel::link_id::__private::new_unchecked;
    use crate::core::link_channel::LinkPortType;

    #[test]
    fn test_executor_creation() {
        let executor = SimpleExecutor::new();
        assert_eq!(executor.state(), ExecutorState::Idle);
    }

    #[test]
    fn test_executor_with_graph() {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let executor = SimpleExecutor::with_graph(graph);
        assert_eq!(executor.state(), ExecutorState::Idle);
        assert!(executor.graph.is_some());
    }

    #[test]
    fn test_wired_link_parsing() {
        let link = Link::new(new_unchecked("conn_0"), "proc_a.video", "proc_b.video");
        let wired = WiredLink::new(link, LinkPortType::Video, 16);

        assert_eq!(wired.source_processor(), "proc_a");
        assert_eq!(wired.dest_processor(), "proc_b");
        assert_eq!(wired.id.as_str(), "conn_0");
    }

    #[test]
    fn test_parse_port_address() {
        let (proc_id, port) = wiring::parse_port_address("camera.video_out").unwrap();
        assert_eq!(proc_id, "camera");
        assert_eq!(port, "video_out");

        let result = wiring::parse_port_address("invalid_format");
        assert!(result.is_err());
    }

    #[test]
    fn test_next_processor_id() {
        let mut executor = SimpleExecutor::new();
        assert_eq!(executor.next_processor_id(), "processor_0");
        assert_eq!(executor.next_processor_id(), "processor_1");
        assert_eq!(executor.next_processor_id(), "processor_2");
    }

    #[test]
    fn test_next_link_id() {
        let mut executor = SimpleExecutor::new();
        assert_eq!(executor.next_link_id().as_str(), "link_0");
        assert_eq!(executor.next_link_id().as_str(), "link_1");
    }

    #[test]
    fn test_status_empty_executor() {
        let executor = SimpleExecutor::new();
        let status = executor.status();
        assert!(!status.running);
        assert_eq!(status.processor_count, 0);
        assert_eq!(status.link_count, 0);
        assert!(status.processor_states.is_empty());
    }
}
