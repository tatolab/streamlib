use super::{McpError, Result};
use crate::core::ProcessorRegistry;
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Resource {
    pub uri: String,

    pub name: String,

    pub description: String,

    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResourceContent {
    pub uri: String,

    #[serde(rename = "mimeType")]
    pub mime_type: String,

    pub text: String,
}

pub fn list_resources(registry: Arc<Mutex<ProcessorRegistry>>) -> Result<Vec<Resource>> {
    let registry = registry.lock();
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

pub fn read_resource(
    registry: Arc<Mutex<ProcessorRegistry>>,
    uri: &str,
) -> Result<ResourceContent> {
    let processor_name = uri
        .strip_prefix("processor://")
        .ok_or_else(|| McpError::ResourceNotFound(format!("Invalid URI: {}", uri)))?;

    let registry = registry.lock();
    let registration = registry
        .get(processor_name)
        .ok_or_else(|| McpError::ResourceNotFound(processor_name.to_string()))?;

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
        let descriptor = ProcessorDescriptor::new("TestProcessor", "A test processor");
        registry.lock().register(descriptor).unwrap();
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

    #[test]
    fn test_audio_requirements_in_descriptor() {
        use crate::core::AudioRequirements;

        let registry = create_test_registry();

        let descriptor = ProcessorDescriptor::new("AudioProcessor", "Test audio processor")
            .with_audio_requirements(AudioRequirements::required(2048, 48000, 2));

        registry.lock().register(descriptor).unwrap();

        let content = read_resource(registry, "processor://AudioProcessor").unwrap();

        let json: serde_json::Value = serde_json::from_str(&content.text).unwrap();

        assert!(
            json.get("audio_requirements").is_some(),
            "audio_requirements should be present in JSON"
        );

        let audio_req = &json["audio_requirements"];
        assert_eq!(audio_req["required_buffer_size"], 2048);
        assert_eq!(audio_req["supported_sample_rates"][0], 48000);
        assert_eq!(audio_req["required_channels"], 2);
    }
}
