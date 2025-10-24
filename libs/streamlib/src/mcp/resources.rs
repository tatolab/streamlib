//! MCP Resources - Processor Discovery
//!
//! Resources expose processor descriptors as read-only data.
//! AI agents can query available processors and their capabilities.

use super::{McpError, Result};
use crate::core::ProcessorRegistry;
use std::sync::{Arc, Mutex};

/// MCP Resource representation
#[derive(Debug, Clone, serde::Serialize)]
pub struct Resource {
    /// Resource URI (e.g., "processor://CameraProcessor")
    pub uri: String,

    /// Human-readable name
    pub name: String,

    /// Resource description
    pub description: String,

    /// MIME type (always "application/json" for processors)
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

/// MCP Resource content
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResourceContent {
    /// Resource URI
    pub uri: String,

    /// MIME type
    #[serde(rename = "mimeType")]
    pub mime_type: String,

    /// Content (JSON string of processor descriptor)
    pub text: String,
}

/// List all available processor resources
///
/// This is called when an AI agent queries "resources/list"
pub fn list_resources(registry: Arc<Mutex<ProcessorRegistry>>) -> Result<Vec<Resource>> {
    let registry = registry.lock().unwrap();
    let descriptors = registry.list();

    Ok(descriptors
        .into_iter()
        .map(|desc| Resource {
            uri: format!("processor://{}", desc.name),
            name: desc.name.clone(),
            description: desc.description.clone(),
            mime_type: "application/json".to_string(),
        })
        .collect())
}

/// Read a specific processor resource
///
/// This is called when an AI agent queries "resources/read"
/// with a URI like "processor://CameraProcessor"
pub fn read_resource(
    registry: Arc<Mutex<ProcessorRegistry>>,
    uri: &str,
) -> Result<ResourceContent> {
    // Parse URI: "processor://ProcessorName"
    let processor_name = uri
        .strip_prefix("processor://")
        .ok_or_else(|| McpError::ResourceNotFound(format!("Invalid URI: {}", uri)))?;

    // Get descriptor from registry
    let registry = registry.lock().unwrap();
    let registration = registry
        .get(processor_name)
        .ok_or_else(|| McpError::ResourceNotFound(processor_name.to_string()))?;

    // Serialize descriptor to JSON
    let json = registration
        .descriptor
        .to_json()
        .map_err(|e| McpError::Protocol(format!("Failed to serialize descriptor: {}", e)))?;

    Ok(ResourceContent {
        uri: uri.to_string(),
        mime_type: "application/json".to_string(),
        text: json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ProcessorDescriptor, ProcessorRegistry};

    fn create_test_registry() -> Arc<Mutex<ProcessorRegistry>> {
        let registry = ProcessorRegistry::new();
        Arc::new(Mutex::new(registry))
    }

    fn register_test_processor(registry: &Arc<Mutex<ProcessorRegistry>>) {
        use std::sync::Arc as StdArc;

        let descriptor = ProcessorDescriptor::new("TestProcessor", "A test processor");
        let factory = StdArc::new(|| Err(crate::core::StreamError::Configuration("Test".into())));

        registry
            .lock()
            .unwrap()
            .register(descriptor, factory)
            .unwrap();
    }

    #[test]
    fn test_list_empty_resources() {
        let registry = create_test_registry();
        let resources = list_resources(registry).unwrap();
        assert_eq!(resources.len(), 0);
    }

    #[test]
    fn test_list_resources() {
        let registry = create_test_registry();
        register_test_processor(&registry);

        let resources = list_resources(registry).unwrap();
        assert_eq!(resources.len(), 1);

        let resource = &resources[0];
        assert_eq!(resource.uri, "processor://TestProcessor");
        assert_eq!(resource.name, "TestProcessor");
        assert_eq!(resource.mime_type, "application/json");
    }

    #[test]
    fn test_read_resource() {
        let registry = create_test_registry();
        register_test_processor(&registry);

        let content = read_resource(registry, "processor://TestProcessor").unwrap();
        assert_eq!(content.uri, "processor://TestProcessor");
        assert_eq!(content.mime_type, "application/json");
        assert!(content.text.contains("TestProcessor"));
    }

    #[test]
    fn test_read_nonexistent_resource() {
        let registry = create_test_registry();
        let result = read_resource(registry, "processor://NonExistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_uri() {
        let registry = create_test_registry();
        let result = read_resource(registry, "invalid://uri");
        assert!(result.is_err());
    }
}
