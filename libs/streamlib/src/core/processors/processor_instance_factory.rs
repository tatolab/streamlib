// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::RwLock;

use crate::core::error::{Result, StreamError};
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::{DynGeneratedProcessor, GeneratedProcessor};
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};
use crate::core::ProcessorDescriptor;

/// A created processor instance for runtime use.
pub type ProcessorInstance = Box<dyn DynGeneratedProcessor + Send>;

/// Types used by macro-generated code. Not for direct use.
pub mod macro_codegen {
    use super::ProcessorInstanceFactory;

    /// Registration entry for auto-registration of processor factories via inventory.
    pub struct FactoryRegistration {
        pub register_fn: fn(&ProcessorInstanceFactory),
    }

    inventory::collect!(FactoryRegistration);
}

/// Factory function signature for creating processor instances.
///
/// Used by `register_dynamic()` for runtime processor registration from plugins.
pub type DynamicProcessorConstructorFn =
    Box<dyn Fn(&ProcessorNode) -> Result<ProcessorInstance> + Send + Sync>;

mod private {
    /// Factory function signature for creating processors (internal alias).
    pub type ConstructorFn = super::DynamicProcessorConstructorFn;
}

/// Result of processor registration.
#[derive(Debug, Clone)]
pub struct RegisterResult {
    /// Number of processors registered.
    pub count: usize,
}

/// Factory for compile-time registered Rust processors.
pub struct ProcessorInstanceFactory {
    constructors: RwLock<HashMap<String, private::ConstructorFn>>,
    port_info: RwLock<HashMap<String, (Vec<PortInfo>, Vec<PortInfo>)>>,
    descriptors: RwLock<HashMap<String, ProcessorDescriptor>>,
}

/// Global processor registry for runtime lookups.
/// Auto-registers all processors collected via inventory on first access.
pub static PROCESSOR_REGISTRY: LazyLock<ProcessorInstanceFactory> = LazyLock::new(|| {
    let factory = ProcessorInstanceFactory::new();
    // Auto-register all processors; ignore errors here (StreamRuntime::new checks for empty registry)
    for registration in inventory::iter::<macro_codegen::FactoryRegistration> {
        (registration.register_fn)(&factory);
    }
    factory
});

impl Default for ProcessorInstanceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessorInstanceFactory {
    pub fn new() -> Self {
        Self {
            constructors: RwLock::new(HashMap::new()),
            port_info: RwLock::new(HashMap::new()),
            descriptors: RwLock::new(HashMap::new()),
        }
    }

    /// Register all processors collected via inventory at link time.
    /// Safe to call multiple times - duplicates are skipped.
    /// Returns registration result with count, or an error if registration failed.
    pub fn register_all_processors(&self) -> crate::Result<RegisterResult> {
        for registration in inventory::iter::<macro_codegen::FactoryRegistration> {
            (registration.register_fn)(self);
        }
        let count = self.constructors.read().len();
        if count == 0 {
            return Err(crate::core::StreamError::RegistryFailed(
                "No processors registered. Ensure processor crates are linked and use #[streamlib::processor]".into()
            ));
        }
        Ok(RegisterResult { count })
    }

    /// Register a processor type with its constructor.
    pub fn register<P>(&self)
    where
        P: GeneratedProcessor + 'static,
        P::Config: for<'de> serde::Deserialize<'de> + Default,
    {
        let descriptor = match <P as GeneratedProcessor>::descriptor() {
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

        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs, outputs));

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        let constructor: private::ConstructorFn = Box::new(move |node: &ProcessorNode| {
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
            Ok(Box::new(processor) as ProcessorInstance)
        });

        {
            let mut constructors = self.constructors.write();
            if constructors.contains_key(&type_name) {
                tracing::debug!(
                    "Processor '{}' already registered, skipping duplicate",
                    type_name
                );
                return;
            }
            constructors.insert(type_name.clone(), constructor);
        }

        tracing::info!("[register] new processor type registered '{}'", type_name);

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name,
            }),
        );
    }

    /// Register a processor dynamically at runtime (for plugins).
    ///
    /// Unlike `register<P>()` which requires compile-time knowledge of the processor type,
    /// this method accepts pre-built descriptor and constructor for runtime registration.
    ///
    /// # Arguments
    /// * `descriptor` - Processor metadata including name, ports, and config schema
    /// * `constructor` - Factory function that creates processor instances
    ///
    /// # Returns
    /// Error if a processor with the same name is already registered.
    pub fn register_dynamic(
        &self,
        descriptor: ProcessorDescriptor,
        constructor: private::ConstructorFn,
    ) -> Result<()> {
        let type_name = descriptor.name.clone();

        // Check for duplicate registration
        if self.constructors.read().contains_key(&type_name) {
            return Err(StreamError::Configuration(format!(
                "Processor '{}' already registered",
                type_name
            )));
        }

        // Build port info from descriptor
        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs, outputs));

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        self.constructors
            .write()
            .insert(type_name.clone(), constructor);

        tracing::info!(
            "[register_dynamic] new processor type registered '{}'",
            type_name
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name,
            }),
        );

        Ok(())
    }

    /// Register a processor descriptor without a constructor.
    ///
    /// Used for subprocess processors (Python, TypeScript) where no Rust-side
    /// `ProcessorInstance` is created. The graph needs the descriptor and port info
    /// for validation and wiring, but `create()` will return an error if called.
    pub fn register_descriptor_only(&self, descriptor: ProcessorDescriptor) -> Result<()> {
        let type_name = descriptor.name.clone();

        if self.descriptors.read().contains_key(&type_name) {
            return Err(StreamError::Configuration(format!(
                "Processor '{}' already registered",
                type_name
            )));
        }

        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs, outputs));

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        // No constructor registered - create() will fail with ProcessorNotFound,
        // which is correct since subprocess processors are never instantiated in Rust.

        tracing::info!(
            "[register_descriptor_only] subprocess processor type registered '{}'",
            type_name
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name,
            }),
        );

        Ok(())
    }

    pub fn can_create(&self, processor_type: &str) -> bool {
        self.constructors.read().contains_key(processor_type)
    }

    pub fn create(&self, node: &ProcessorNode) -> Result<ProcessorInstance> {
        let constructors = self.constructors.read();
        let constructor = constructors.get(&node.processor_type).ok_or_else(|| {
            StreamError::ProcessorNotFound(format!(
                "No factory registered for processor type '{}'",
                node.processor_type
            ))
        })?;

        constructor(node)
    }

    pub fn port_info(&self, processor_type: &str) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.port_info.read().get(processor_type).cloned()
    }

    pub fn is_registered(&self, processor_type: &str) -> bool {
        self.constructors.read().contains_key(processor_type)
    }

    /// Get the descriptor for a processor type, if registered.
    pub fn descriptor(&self, processor_type: &str) -> Option<ProcessorDescriptor> {
        self.descriptors.read().get(processor_type).cloned()
    }

    /// List all registered processor types with their full descriptors.
    pub fn list_registered(&self) -> Vec<ProcessorDescriptor> {
        self.descriptors.read().values().cloned().collect()
    }
}
