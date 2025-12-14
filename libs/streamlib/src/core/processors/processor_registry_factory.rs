// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;

use parking_lot::RwLock;

use crate::core::error::{Result, StreamError};
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::{DynProcessor, Processor};
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};

/// A created processor instance for runtime use.
pub type ProcessorInstance = Box<dyn DynProcessor + Send>;

/// Types used by macro-generated code. Not for direct use.
pub mod macro_codegen {
    use super::ProcessorRegistryFactory;

    /// Registration entry for auto-registration of processor factories via inventory.
    pub struct FactoryRegistration {
        pub register_fn: fn(&ProcessorRegistryFactory),
    }

    inventory::collect!(FactoryRegistration);
}

mod private {
    use super::{ProcessorInstance, ProcessorNode, Result};

    /// Factory function signature for creating processors.
    pub type ConstructorFn = Box<dyn Fn(&ProcessorNode) -> Result<ProcessorInstance> + Send + Sync>;
}

/// Factory for compile-time registered Rust processors.
pub struct ProcessorRegistryFactory {
    constructors: RwLock<HashMap<String, private::ConstructorFn>>,
    port_info: RwLock<HashMap<String, (Vec<PortInfo>, Vec<PortInfo>)>>,
}

impl Default for ProcessorRegistryFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessorRegistryFactory {
    pub fn new() -> Self {
        let instance = Self {
            constructors: RwLock::new(HashMap::new()),
            port_info: RwLock::new(HashMap::new()),
        };
        instance.register_all_processors();
        instance
    }

    /// Register all processors collected via inventory at link time.
    /// Safe to call multiple times - duplicates are skipped with a warning.
    pub fn register_all_processors(&self) {
        for registration in inventory::iter::<macro_codegen::FactoryRegistration> {
            (registration.register_fn)(self);
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

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs, outputs));

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
}
