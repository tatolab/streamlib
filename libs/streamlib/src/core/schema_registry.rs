// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Global schema registry for compile-time and runtime schema registration.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

use crate::core::graph::LinkCapacity;
use crate::core::links::{
    LinkBufferReadMode, LinkInstance, LinkInstanceCreationResult, LinkPortMessage,
};
use crate::core::schema::{DataFrameSchemaField, SemanticVersion};
use crate::core::StreamError;

/// Global schema registry singleton.
pub static SCHEMA_REGISTRY: LazyLock<SchemaRegistry> = LazyLock::new(SchemaRegistry::new);

/// Static schema field for compile-time registration.
pub struct StaticSchemaField {
    pub name: &'static str,
    pub primitive: crate::core::schema::PrimitiveType,
    pub shape: &'static [usize],
    pub serializable: bool,
}

impl StaticSchemaField {
    /// Convert to runtime DataFrameSchemaField.
    pub fn to_field(&self) -> DataFrameSchemaField {
        DataFrameSchemaField {
            name: self.name.to_string(),
            primitive: self.primitive,
            shape: self.shape.to_vec(),
        }
    }
}

/// Factory trait for creating typed link instances from schema.
pub trait SchemaLinkFactory: Send + Sync {
    /// Create a link instance with the given capacity.
    fn create_link_instance(
        &self,
        capacity: LinkCapacity,
    ) -> crate::core::Result<LinkInstanceCreationResult>;
}

/// Schema registration entry for inventory collection.
pub struct SchemaRegistration {
    pub name: &'static str,
    pub version: SemanticVersion,
    pub fields: &'static [StaticSchemaField],
    pub read_behavior: LinkBufferReadMode,
    pub factory: &'static dyn SchemaLinkFactory,
}

// Safety: SchemaRegistration contains only static references and Send+Sync types
unsafe impl Send for SchemaRegistration {}
unsafe impl Sync for SchemaRegistration {}

inventory::collect!(SchemaRegistration);

/// Entry in the schema registry.
pub struct SchemaEntry {
    pub name: String,
    pub version: SemanticVersion,
    pub fields: Vec<DataFrameSchemaField>,
    pub read_behavior: LinkBufferReadMode,
    /// Factory for creating LinkInstance. None for runtime-only schemas.
    pub link_factory: Option<Arc<dyn SchemaLinkFactory>>,
}

impl SchemaEntry {
    /// Check if this schema is compatible with another.
    pub fn compatible_with(&self, other: &SchemaEntry) -> bool {
        // Same name required
        if self.name != other.name {
            return false;
        }
        // Major version must match for compatibility
        self.version.major == other.version.major
    }
}

/// Global registry for schemas.
pub struct SchemaRegistry {
    schemas: RwLock<HashMap<String, Arc<SchemaEntry>>>,
    initialized: RwLock<bool>,
}

impl SchemaRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            schemas: RwLock::new(HashMap::new()),
            initialized: RwLock::new(false),
        }
    }

    /// Initialize the registry from inventory-collected schemas.
    /// Called automatically on first access.
    fn ensure_initialized(&self) {
        let mut initialized = self.initialized.write();
        if *initialized {
            return;
        }

        let mut schemas = self.schemas.write();
        for registration in inventory::iter::<SchemaRegistration> {
            let entry = SchemaEntry {
                name: registration.name.to_string(),
                version: registration.version.clone(),
                fields: registration.fields.iter().map(|f| f.to_field()).collect(),
                read_behavior: registration.read_behavior,
                link_factory: Some(Arc::new(StaticLinkFactory {
                    factory: registration.factory,
                })),
            };

            if schemas.contains_key(&entry.name) {
                tracing::warn!(
                    "Schema '{}' already registered, skipping duplicate",
                    entry.name
                );
                continue;
            }

            tracing::debug!("Registered schema: {} v{}", entry.name, entry.version);
            schemas.insert(entry.name.clone(), Arc::new(entry));
        }

        *initialized = true;
    }

    /// Register a runtime schema (e.g., from Python).
    pub fn register_runtime(&self, entry: SchemaEntry) -> crate::core::Result<()> {
        self.ensure_initialized();

        let mut schemas = self.schemas.write();
        if schemas.contains_key(&entry.name) {
            return Err(StreamError::Configuration(format!(
                "Schema '{}' already registered",
                entry.name
            )));
        }

        tracing::info!(
            "Registered runtime schema: {} v{}",
            entry.name,
            entry.version
        );
        schemas.insert(entry.name.clone(), Arc::new(entry));
        Ok(())
    }

    /// Register a runtime DataFrame schema with automatic link factory.
    /// This is the recommended way to register schemas from Python or other dynamic sources.
    pub fn register_dataframe_schema(
        &self,
        name: String,
        version: SemanticVersion,
        fields: Vec<DataFrameSchemaField>,
        read_behavior: LinkBufferReadMode,
    ) -> crate::core::Result<()> {
        let entry = SchemaEntry {
            name,
            version,
            fields,
            read_behavior,
            link_factory: Some(Arc::new(DataFrameLinkFactory)),
        };
        self.register_runtime(entry)
    }

    /// Get a schema by name.
    pub fn get(&self, name: &str) -> Option<Arc<SchemaEntry>> {
        self.ensure_initialized();
        self.schemas.read().get(name).cloned()
    }

    /// Check if two schemas are compatible by name.
    pub fn compatible(&self, a: &str, b: &str) -> bool {
        self.ensure_initialized();

        let schemas = self.schemas.read();
        match (schemas.get(a), schemas.get(b)) {
            (Some(schema_a), Some(schema_b)) => schema_a.compatible_with(schema_b),
            _ => false,
        }
    }

    /// List all registered schemas.
    pub fn list(&self) -> Vec<Arc<SchemaEntry>> {
        self.ensure_initialized();
        self.schemas.read().values().cloned().collect()
    }

    /// Check if a schema is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.ensure_initialized();
        self.schemas.read().contains_key(name)
    }

    /// Get the number of registered schemas.
    pub fn len(&self) -> usize {
        self.ensure_initialized();
        self.schemas.read().len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Create a link instance for a schema.
    pub fn create_link_instance(
        &self,
        schema_name: &str,
        capacity: LinkCapacity,
    ) -> crate::core::Result<LinkInstanceCreationResult> {
        let entry = self.get(schema_name).ok_or_else(|| {
            StreamError::Configuration(format!("Schema '{}' not found in registry", schema_name))
        })?;

        let factory = entry.link_factory.as_ref().ok_or_else(|| {
            StreamError::Configuration(format!(
                "Schema '{}' does not support link creation (runtime-only schema)",
                schema_name
            ))
        })?;

        factory.create_link_instance(capacity)
    }
}

impl Default for SchemaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapper to convert static factory reference to Arc-compatible factory.
struct StaticLinkFactory {
    factory: &'static dyn SchemaLinkFactory,
}

impl SchemaLinkFactory for StaticLinkFactory {
    fn create_link_instance(
        &self,
        capacity: LinkCapacity,
    ) -> crate::core::Result<LinkInstanceCreationResult> {
        self.factory.create_link_instance(capacity)
    }
}

/// Factory for creating DataFrame link instances for runtime schemas.
/// Use this when registering schemas from Python or other dynamic sources.
pub struct DataFrameLinkFactory;

impl SchemaLinkFactory for DataFrameLinkFactory {
    fn create_link_instance(
        &self,
        capacity: LinkCapacity,
    ) -> crate::core::Result<LinkInstanceCreationResult> {
        create_typed_link_instance::<crate::core::frames::DataFrame>(capacity)
    }
}

/// Helper to create a typed link instance. Used by generated schema code.
pub fn create_typed_link_instance<T>(
    capacity: LinkCapacity,
) -> crate::core::Result<LinkInstanceCreationResult>
where
    T: LinkPortMessage + 'static,
{
    use crate::core::graph::LinkTypeInfoComponent;

    let instance = LinkInstance::<T>::new(capacity);
    let data_writer = instance.create_link_output_data_writer();
    let data_reader = instance.create_link_input_data_reader();

    Ok(LinkInstanceCreationResult {
        instance: Box::new(instance),
        type_info: LinkTypeInfoComponent::new::<T>(capacity),
        data_writer: Box::new(data_writer),
        data_reader: Box::new(data_reader),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_registry_creation() {
        let registry = SchemaRegistry::new();
        // Registry starts empty (before inventory initialization)
        assert!(registry.schemas.read().is_empty());
    }

    #[test]
    fn test_schema_compatibility() {
        let entry_a = SchemaEntry {
            name: "TestSchema".to_string(),
            version: SemanticVersion::new(1, 0, 0),
            fields: vec![],
            read_behavior: LinkBufferReadMode::SkipToLatest,
            link_factory: None,
        };

        let entry_b = SchemaEntry {
            name: "TestSchema".to_string(),
            version: SemanticVersion::new(1, 1, 0),
            fields: vec![],
            read_behavior: LinkBufferReadMode::SkipToLatest,
            link_factory: None,
        };

        let entry_c = SchemaEntry {
            name: "TestSchema".to_string(),
            version: SemanticVersion::new(2, 0, 0),
            fields: vec![],
            read_behavior: LinkBufferReadMode::SkipToLatest,
            link_factory: None,
        };

        // Same major version = compatible
        assert!(entry_a.compatible_with(&entry_b));
        // Different major version = incompatible
        assert!(!entry_a.compatible_with(&entry_c));
    }

    #[test]
    fn test_runtime_registration() {
        let registry = SchemaRegistry::new();

        let entry = SchemaEntry {
            name: "RuntimeTestSchema".to_string(),
            version: SemanticVersion::new(1, 0, 0),
            fields: vec![],
            read_behavior: LinkBufferReadMode::SkipToLatest,
            link_factory: None,
        };

        registry.register_runtime(entry).unwrap();
        assert!(registry.contains("RuntimeTestSchema"));

        // Duplicate registration should fail
        let entry2 = SchemaEntry {
            name: "RuntimeTestSchema".to_string(),
            version: SemanticVersion::new(1, 0, 0),
            fields: vec![],
            read_behavior: LinkBufferReadMode::SkipToLatest,
            link_factory: None,
        };

        assert!(registry.register_runtime(entry2).is_err());
    }
}
