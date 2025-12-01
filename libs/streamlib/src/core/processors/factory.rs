//! Processor instantiation factories.
//!
//! Factories create processor instances from [`ProcessorNode`] specifications.
//! This enables the runtime to instantiate processors from graph metadata.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::error::{Result, StreamError};
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::Processor;

use super::DynProcessor;

/// Boxed dynamic processor for runtime use.
pub type BoxedProcessor = Box<dyn DynProcessor + Send>;

/// Factory function signature for creating processors.
type ConstructorFn = Box<dyn Fn(&ProcessorNode) -> Result<BoxedProcessor> + Send + Sync>;

/// Creates processor instances from node specifications.
pub trait ProcessorNodeFactory: Send + Sync {
    /// Check if this factory can create the given processor type.
    fn can_create(&self, processor_type: &str) -> bool;

    /// Create a processor instance from a node specification.
    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor>;

    /// Get port information for a processor type (inputs, outputs).
    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)>;
}

/// Factory for compile-time registered Rust processors.
pub struct RegistryBackedFactory {
    constructors: RwLock<HashMap<String, ConstructorFn>>,
    port_info: RwLock<HashMap<String, (Vec<PortInfo>, Vec<PortInfo>)>>,
}

impl Default for RegistryBackedFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryBackedFactory {
    pub fn new() -> Self {
        Self {
            constructors: RwLock::new(HashMap::new()),
            port_info: RwLock::new(HashMap::new()),
        }
    }

    /// Register a processor type with its constructor.
    pub fn register<P>(&self)
    where
        P: Processor + 'static,
        P::Config: for<'de> serde::Deserialize<'de> + Default,
    {
        let descriptor = match <P as Processor>::descriptor() {
            Some(d) => d,
            None => {
                tracing::warn!(
                    "Processor {} has no descriptor, skipping registration",
                    std::any::type_name::<P>()
                );
                return;
            }
        };

        let type_name = descriptor.name.clone();

        // Extract port info
        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.name.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.name.clone(),
                port_kind: Default::default(),
            })
            .collect();

        // Store port info
        self.port_info
            .write()
            .insert(type_name.clone(), (inputs, outputs));

        // Store constructor
        let constructor: ConstructorFn = Box::new(move |node: &ProcessorNode| {
            let config: P::Config = match &node.config {
                Some(json) => serde_json::from_value(json.clone()).map_err(|e| {
                    StreamError::Configuration(format!(
                        "Failed to deserialize config for '{}': {}",
                        node.id, e
                    ))
                })?,
                None => P::Config::default(),
            };

            let processor = P::from_config(config)?;
            Ok(Box::new(processor) as BoxedProcessor)
        });

        self.constructors
            .write()
            .insert(type_name.clone(), constructor);

        tracing::debug!("Registered processor factory for '{}'", type_name);
    }

    /// Check if a processor type is registered.
    pub fn is_registered(&self, processor_type: &str) -> bool {
        self.constructors.read().contains_key(processor_type)
    }
}

impl ProcessorNodeFactory for RegistryBackedFactory {
    fn can_create(&self, processor_type: &str) -> bool {
        self.constructors.read().contains_key(processor_type)
    }

    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor> {
        let constructors = self.constructors.read();
        let constructor = constructors.get(&node.processor_type).ok_or_else(|| {
            StreamError::ProcessorNotFound(format!(
                "No factory registered for processor type '{}'",
                node.processor_type
            ))
        })?;

        constructor(node)
    }

    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.port_info.read().get(processor_type).cloned()
    }
}

/// Composite factory that delegates to multiple factory sources.
pub struct CompositeFactory {
    factories: Vec<Arc<dyn ProcessorNodeFactory>>,
}

impl Default for CompositeFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositeFactory {
    pub fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Add a factory source.
    pub fn add_factory(&mut self, factory: Arc<dyn ProcessorNodeFactory>) {
        self.factories.push(factory);
    }

    /// Create a composite factory with the given sources.
    pub fn with_factories(factories: Vec<Arc<dyn ProcessorNodeFactory>>) -> Self {
        Self { factories }
    }
}

impl ProcessorNodeFactory for CompositeFactory {
    fn can_create(&self, processor_type: &str) -> bool {
        self.factories.iter().any(|f| f.can_create(processor_type))
    }

    fn create(&self, node: &ProcessorNode) -> Result<BoxedProcessor> {
        for factory in &self.factories {
            if factory.can_create(&node.processor_type) {
                return factory.create(node);
            }
        }

        Err(StreamError::ProcessorNotFound(format!(
            "No factory can create processor type '{}'",
            node.processor_type
        )))
    }

    fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        for factory in &self.factories {
            if let Some(info) = factory.port_info(processor_type) {
                return Some(info);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_factory_empty() {
        let factory = RegistryBackedFactory::new();
        assert!(!factory.can_create("UnknownProcessor"));
    }

    #[test]
    fn test_composite_factory_empty() {
        let factory = CompositeFactory::new();
        assert!(!factory.can_create("AnyProcessor"));
    }

    #[test]
    fn test_composite_factory_delegates() {
        let registry = Arc::new(RegistryBackedFactory::new());
        let composite = CompositeFactory::with_factories(vec![registry]);

        // Should delegate can_create check
        assert!(!composite.can_create("UnknownProcessor"));
    }
}
