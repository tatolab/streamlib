use crate::core::{Result, bus::PortMessage};

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

    pub fn add_processor_with_config<P>(&mut self, config: P::Config) -> Result<ProcessorHandle>
    where
        P: crate::core::traits::StreamProcessor + 'static,
    {
        self.inner.add_processor_with_config::<P>(config)
    }

    pub fn add_processor<P>(&mut self) -> Result<ProcessorHandle>
    where
        P: crate::core::traits::StreamProcessor + 'static,
    {
        self.inner.add_processor::<P>()
    }

    pub fn connect<T: PortMessage>(
        &mut self,
        output: crate::core::handles::OutputPortRef<T>,
        input: crate::core::handles::InputPortRef<T>,
    ) -> Result<()> {
        self.inner.connect(output, input)
    }

    pub fn start(&mut self) -> Result<()> {
        self.inner.start()
    }

    pub fn run(&mut self) -> Result<()> {
        self.inner.run()
    }

    pub fn stop(&mut self) -> Result<()> {
        self.inner.stop()
    }

    pub fn audio_config(&self) -> AudioContext {
        self.inner.audio_config()
    }

    pub fn set_audio_config(&mut self, config: AudioContext) {
        self.inner.set_audio_config(config)
    }

}
