use crate::core::error::{Result, StreamError};
use crate::core::executor::{ExecutorLifecycle, ExecutorState, GraphCompiler};

use super::SimpleExecutor;

impl crate::core::pubsub::EventListener for SimpleExecutor {
    fn on_event(&mut self, event: &crate::core::pubsub::Event) -> Result<()> {
        use crate::core::pubsub::{Event, RuntimeEvent};

        if let Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted) = event {
            tracing::info!("Executor received RuntimeStarted, triggering compile...");
            self.compile()?;
        }
        Ok(())
    }
}

impl ExecutorLifecycle for SimpleExecutor {
    fn state(&self) -> ExecutorState {
        self.state
    }

    fn start(&mut self) -> Result<()> {
        if self.state == ExecutorState::Running {
            tracing::debug!("Executor already running");
            return Ok(());
        }

        tracing::info!("Starting executor...");

        #[cfg(target_os = "macos")]
        let is_standalone = {
            use objc2::MainThreadMarker;
            use objc2_app_kit::NSApplication;

            if let Some(mtm) = MainThreadMarker::new() {
                let app = NSApplication::sharedApplication(mtm);
                !app.isRunning()
            } else {
                // Not on main thread - can't set up NSApplication
                false
            }
        };

        #[cfg(target_os = "macos")]
        if is_standalone {
            crate::apple::runtime_ext::setup_macos_app();
            crate::apple::runtime_ext::install_macos_shutdown_handler();
        }

        self.state = ExecutorState::Running;

        #[cfg(target_os = "macos")]
        {
            self.is_macos_standalone = is_standalone;
        }

        tracing::info!("Executor started (state=Running)");

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if self.state == ExecutorState::Idle {
            tracing::debug!("Executor already stopped");
            return Ok(());
        }

        if self.state != ExecutorState::Running && self.state != ExecutorState::Paused {
            return Err(StreamError::Runtime(format!(
                "Cannot stop executor in state {:?}",
                self.state
            )));
        }

        tracing::info!("Stopping executor...");

        let processor_ids: Vec<_> = self
            .execution_graph
            .as_ref()
            .map(|eg| eg.processor_ids().cloned().collect())
            .unwrap_or_default();

        for id in processor_ids {
            if let Err(e) = self.shutdown_processor(&id) {
                tracing::warn!("Error shutting down processor {}: {}", id, e);
            }
        }

        if let Some(exec_graph) = &mut self.execution_graph {
            exec_graph.clear_runtime_state();
        }

        self.state = ExecutorState::Idle;
        tracing::info!("Executor stopped");
        Ok(())
    }

    fn pause(&mut self) -> Result<()> {
        if self.state == ExecutorState::Paused {
            tracing::debug!("Executor already paused");
            return Ok(());
        }

        if self.state != ExecutorState::Running {
            return Err(StreamError::Runtime(format!(
                "Cannot pause executor in state {:?}",
                self.state
            )));
        }

        tracing::info!("Pausing executor...");
        self.state = ExecutorState::Paused;
        tracing::info!("Executor paused");
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if self.state == ExecutorState::Running {
            tracing::debug!("Executor already running");
            return Ok(());
        }

        if self.state != ExecutorState::Paused {
            return Err(StreamError::Runtime(format!(
                "Cannot resume executor in state {:?}",
                self.state
            )));
        }

        tracing::info!("Resuming executor...");
        self.state = ExecutorState::Running;
        tracing::info!("Executor resumed");
        Ok(())
    }
}
