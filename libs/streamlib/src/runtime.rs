use crate::core::{Result, ports::PortMessage};

pub use crate::core::runtime::AudioContext;
pub use crate::core::handles::ProcessorHandle;

pub struct StreamRuntime {
    inner: crate::core::StreamRuntime,
}

impl StreamRuntime {
    pub fn new() -> Self {
        let mut inner = crate::core::StreamRuntime::new();

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            crate::apple::configure_macos_event_loop(&mut inner);
        }

        Self { inner }
    }

    /// Add a processor with config (works both before and after start())
    pub async fn add_element_with_config<P>(&mut self, config: P::Config) -> Result<ProcessorHandle>
    where
        P: crate::core::traits::StreamProcessor + 'static,
    {
        // Check if runtime is started
        if self.inner.is_running() {
            // Runtime is running - add dynamically
            let element = P::from_config(config)?;
            let id = self.inner.add_processor_runtime(Box::new(element)).await?;
            Ok(ProcessorHandle::new(id))
        } else {
            // Runtime not started yet - add to pending list (no await needed)
            self.inner.add_processor_with_config::<P>(config)
        }
    }

    pub fn connect<T: PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<()> {
        self.inner.connect(output, input)
    }

    pub async fn start(&mut self) -> Result<()> {
        self.inner.start().await
    }

    pub async fn run(&mut self) -> Result<()> {
        self.inner.run().await
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.inner.stop().await
    }

    pub fn audio_config(&self) -> AudioContext {
        self.inner.audio_config()
    }

    pub fn set_audio_config(&mut self, config: AudioContext) {
        self.inner.set_audio_config(config)
    }

}
