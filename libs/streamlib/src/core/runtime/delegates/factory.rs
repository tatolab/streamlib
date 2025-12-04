//! Default factory delegate implementation.

use crate::core::delegates::FactoryDelegate;
use crate::core::error::Result;
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::{BoxedProcessor, ProcessorNodeFactory, RegistryBackedFactory};

/// Default factory implementation using the processor registry.
pub struct DefaultFactory {
    inner: RegistryBackedFactory,
}

impl DefaultFactory {
    /// Create a new default factory.
    pub fn new() -> Self {
        Self {
            inner: RegistryBackedFactory::new(),
        }
    }

    /// Get a reference to the inner registry-backed factory.
    pub fn inner(&self) -> &RegistryBackedFactory {
        &self.inner
    }

    /// Register a processor type with the factory.
    pub fn register<P>(&self)
    where
        P: crate::core::processors::Processor + 'static,
        P::Config: serde::Serialize + for<'de> serde::Deserialize<'de> + Default,
    {
        self.inner.register::<P>();
    }
}

impl Default for DefaultFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl FactoryDelegate for DefaultFactory {
    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor> {
        self.inner.create(node)
    }

    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.inner.port_info(processor_type)
    }

    fn can_create(&self, processor_type: &str) -> bool {
        self.inner.can_create(processor_type)
    }
}

/// Wrapper to adapt a ProcessorNodeFactory to FactoryDelegate.
pub struct FactoryAdapter<F: ProcessorNodeFactory + Send + Sync> {
    inner: F,
}

impl<F: ProcessorNodeFactory + Send + Sync> FactoryAdapter<F> {
    /// Create a new factory adapter.
    pub fn new(factory: F) -> Self {
        Self { inner: factory }
    }
}

impl<F: ProcessorNodeFactory + Send + Sync> FactoryDelegate for FactoryAdapter<F> {
    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor> {
        self.inner.create(node)
    }

    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.inner.port_info(processor_type)
    }

    fn can_create(&self, processor_type: &str) -> bool {
        self.inner.can_create(processor_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_factory_creation() {
        let factory = DefaultFactory::new();
        // Should not be able to create unknown processor types
        assert!(!factory.can_create("UnknownProcessor"));
    }
}
