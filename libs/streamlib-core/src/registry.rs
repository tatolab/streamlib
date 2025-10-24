//! Processor Registry
//!
//! Runtime registry for dynamic processor discovery and registration.
//! Supports both compile-time (via inventory) and runtime registration.

use crate::{ProcessorDescriptor, StreamError};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Trait for types that provide processor descriptors for inventory registration
///
/// Implement this trait and use `inventory::submit!` to auto-register processors:
///
/// ```no_run
/// use streamlib_core::DescriptorProvider;
///
/// struct MyProcessorDescriptor;
///
/// impl DescriptorProvider for MyProcessorDescriptor {
///     fn descriptor(&self) -> streamlib_core::ProcessorDescriptor {
///         streamlib_core::ProcessorDescriptor::new("MyProcessor", "Does cool things")
///     }
/// }
///
/// // Auto-register at compile time
/// inventory::submit! {
///     &MyProcessorDescriptor as &dyn DescriptorProvider
/// }
/// ```
pub trait DescriptorProvider: Sync {
    fn descriptor(&self) -> ProcessorDescriptor;
}

// Collect all submitted descriptor providers via inventory
inventory::collect!(&'static dyn DescriptorProvider);

/// Processor factory function type
///
/// Takes no arguments and returns a boxed StreamProcessor.
/// Used for creating processor instances from registered descriptors.
pub type ProcessorFactory = Arc<dyn Fn() -> crate::Result<Box<dyn crate::StreamProcessor>> + Send + Sync>;

/// Entry in the processor registry
#[derive(Clone)]
pub struct ProcessorRegistration {
    /// Processor metadata
    pub descriptor: ProcessorDescriptor,

    /// Factory function to create instances
    ///
    /// None for descriptors that are registered without a factory
    /// (e.g., from external tools that just want to advertise capabilities)
    pub factory: Option<ProcessorFactory>,
}

impl ProcessorRegistration {
    /// Create a new registration with both descriptor and factory
    pub fn new(descriptor: ProcessorDescriptor, factory: ProcessorFactory) -> Self {
        Self {
            descriptor,
            factory: Some(factory),
        }
    }

    /// Create a descriptor-only registration (no factory)
    ///
    /// Useful for advertising processor capabilities without providing
    /// the actual implementation (e.g., from Python/TypeScript bindings)
    pub fn descriptor_only(descriptor: ProcessorDescriptor) -> Self {
        Self {
            descriptor,
            factory: None,
        }
    }
}

/// Processor registry for dynamic discovery
///
/// Maintains a runtime registry of available processors, supporting:
/// - Runtime registration (Python, TypeScript, etc.)
/// - Descriptor-only registration (capability advertisement)
/// - Factory-based instantiation
/// - Tag-based filtering
///
/// This registry complements compile-time registration (via inventory crate)
/// and enables dynamic processor loading for AI agents and scripting languages.
pub struct ProcessorRegistry {
    /// Registered processors by name
    processors: HashMap<String, ProcessorRegistration>,
}

impl ProcessorRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            processors: HashMap::new(),
        }
    }

    /// Register a processor with both descriptor and factory
    ///
    /// # Arguments
    /// * `descriptor` - Processor metadata
    /// * `factory` - Function to create processor instances
    ///
    /// # Returns
    /// Error if a processor with the same name is already registered
    pub fn register(
        &mut self,
        descriptor: ProcessorDescriptor,
        factory: ProcessorFactory,
    ) -> crate::Result<()> {
        let name = descriptor.name.clone();

        if self.processors.contains_key(&name) {
            return Err(StreamError::Configuration(
                format!("Processor '{}' is already registered", name)
            ));
        }

        self.processors.insert(
            name,
            ProcessorRegistration::new(descriptor, factory),
        );

        Ok(())
    }

    /// Register a processor descriptor without a factory
    ///
    /// Useful for advertising capabilities from external tools
    /// that don't provide the actual implementation in Rust.
    ///
    /// # Arguments
    /// * `descriptor` - Processor metadata
    ///
    /// # Returns
    /// Error if a processor with the same name is already registered
    pub fn register_descriptor_only(
        &mut self,
        descriptor: ProcessorDescriptor,
    ) -> crate::Result<()> {
        let name = descriptor.name.clone();

        if self.processors.contains_key(&name) {
            return Err(StreamError::Configuration(
                format!("Processor '{}' is already registered", name)
            ));
        }

        self.processors.insert(
            name,
            ProcessorRegistration::descriptor_only(descriptor),
        );

        Ok(())
    }

    /// Get a processor registration by name
    pub fn get(&self, name: &str) -> Option<&ProcessorRegistration> {
        self.processors.get(name)
    }

    /// List all registered processor descriptors
    pub fn list(&self) -> Vec<ProcessorDescriptor> {
        self.processors
            .values()
            .map(|reg| reg.descriptor.clone())
            .collect()
    }

    /// List processors filtered by tag
    pub fn list_by_tag(&self, tag: &str) -> Vec<ProcessorDescriptor> {
        self.processors
            .values()
            .filter(|reg| reg.descriptor.tags.iter().any(|t| t == tag))
            .map(|reg| reg.descriptor.clone())
            .collect()
    }

    /// Create a processor instance by name
    ///
    /// # Arguments
    /// * `name` - Name of the processor to instantiate
    ///
    /// # Returns
    /// A boxed StreamProcessor instance, or an error if:
    /// - Processor not found
    /// - Processor has no factory
    /// - Factory function fails
    pub fn create_instance(&self, name: &str) -> crate::Result<Box<dyn crate::StreamProcessor>> {
        let registration = self.get(name)
            .ok_or_else(|| StreamError::Configuration(
                format!("Processor '{}' not found in registry", name)
            ))?;

        let factory = registration.factory.as_ref()
            .ok_or_else(|| StreamError::Configuration(
                format!("Processor '{}' has no factory (descriptor-only registration)", name)
            ))?;

        factory()
    }

    /// Check if a processor is registered
    pub fn contains(&self, name: &str) -> bool {
        self.processors.contains_key(name)
    }

    /// Remove a processor from the registry
    pub fn unregister(&mut self, name: &str) -> bool {
        self.processors.remove(name).is_some()
    }

    /// Clear all registrations
    pub fn clear(&mut self) {
        self.processors.clear();
    }

    /// Get the number of registered processors
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }
}

impl Default for ProcessorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Global processor registry
///
/// Thread-safe singleton registry for runtime processor registration.
/// Used by Python/TypeScript bindings and AI agent tools.
static GLOBAL_REGISTRY: OnceLock<Arc<Mutex<ProcessorRegistry>>> = OnceLock::new();

/// Get the global processor registry
///
/// On first access, automatically collects and registers all processors
/// that were submitted via `inventory::submit!` at compile-time.
pub fn global_registry() -> Arc<Mutex<ProcessorRegistry>> {
    GLOBAL_REGISTRY
        .get_or_init(|| {
            let mut registry = ProcessorRegistry::new();

            // Auto-register all compile-time submitted descriptors
            for provider in inventory::iter::<&dyn DescriptorProvider> {
                let descriptor = provider.descriptor();
                let name = descriptor.name.clone();

                if let Err(e) = registry.register_descriptor_only(descriptor) {
                    // Log warning but don't fail - allow duplicate submissions to be gracefully ignored
                    tracing::warn!("Failed to auto-register processor '{}': {}", name, e);
                }
            }

            tracing::debug!("Auto-registered {} processors from inventory", registry.len());

            Arc::new(Mutex::new(registry))
        })
        .clone()
}

/// Register a processor in the global registry
///
/// # Arguments
/// * `descriptor` - Processor metadata
/// * `factory` - Function to create processor instances
///
/// # Example
/// ```no_run
/// use streamlib_core::{register_processor, ProcessorDescriptor};
/// use std::sync::Arc;
///
/// let descriptor = ProcessorDescriptor::new("MyProcessor", "Does cool stuff");
///
/// register_processor(
///     descriptor,
///     Arc::new(|| {
///         // Create and return processor instance
///         Ok(Box::new(MyProcessorImpl::new()))
///     })
/// ).unwrap();
/// ```
pub fn register_processor(
    descriptor: ProcessorDescriptor,
    factory: ProcessorFactory,
) -> crate::Result<()> {
    global_registry()
        .lock()
        .unwrap()
        .register(descriptor, factory)
}

/// Register a processor descriptor without a factory
///
/// Useful for advertising processor capabilities from external tools.
///
/// # Arguments
/// * `descriptor` - Processor metadata
pub fn register_processor_descriptor(
    descriptor: ProcessorDescriptor,
) -> crate::Result<()> {
    global_registry()
        .lock()
        .unwrap()
        .register_descriptor_only(descriptor)
}

/// List all registered processors
pub fn list_processors() -> Vec<ProcessorDescriptor> {
    global_registry()
        .lock()
        .unwrap()
        .list()
}

/// List processors filtered by tag
pub fn list_processors_by_tag(tag: &str) -> Vec<ProcessorDescriptor> {
    global_registry()
        .lock()
        .unwrap()
        .list_by_tag(tag)
}

/// Create a processor instance by name
pub fn create_processor(name: &str) -> crate::Result<Box<dyn crate::StreamProcessor>> {
    global_registry()
        .lock()
        .unwrap()
        .create_instance(name)
}

/// Check if a processor is registered
pub fn is_processor_registered(name: &str) -> bool {
    global_registry()
        .lock()
        .unwrap()
        .contains(name)
}

/// Unregister a processor
pub fn unregister_processor(name: &str) -> bool {
    global_registry()
        .lock()
        .unwrap()
        .unregister(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProcessorDescriptor, TimedTick};

    struct MockProcessor;

    impl crate::StreamProcessor for MockProcessor {
        fn process(&mut self, _tick: TimedTick) -> crate::Result<()> {
            Ok(())
        }
    }

    fn create_test_descriptor(name: &str) -> ProcessorDescriptor {
        ProcessorDescriptor::new(name, &format!("{} description", name))
            .with_tags(vec!["test", "mock"])
    }

    fn create_test_factory() -> ProcessorFactory {
        Arc::new(|| Ok(Box::new(MockProcessor) as Box<dyn crate::StreamProcessor>))
    }

    #[test]
    fn test_registry_creation() {
        let registry = ProcessorRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("TestProcessor");
        let factory = create_test_factory();

        registry.register(descriptor.clone(), factory).unwrap();

        assert_eq!(registry.len(), 1);
        assert!(registry.contains("TestProcessor"));

        let registration = registry.get("TestProcessor").unwrap();
        assert_eq!(registration.descriptor.name, "TestProcessor");
        assert!(registration.factory.is_some());
    }

    #[test]
    fn test_register_duplicate() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("TestProcessor");
        let factory = create_test_factory();

        registry.register(descriptor.clone(), factory.clone()).unwrap();

        // Try to register again
        let result = registry.register(descriptor, factory);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already registered"));
    }

    #[test]
    fn test_descriptor_only_registration() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("ExternalProcessor");

        registry.register_descriptor_only(descriptor).unwrap();

        assert_eq!(registry.len(), 1);

        let registration = registry.get("ExternalProcessor").unwrap();
        assert_eq!(registration.descriptor.name, "ExternalProcessor");
        assert!(registration.factory.is_none());
    }

    #[test]
    fn test_list_processors() {
        let mut registry = ProcessorRegistry::new();

        registry.register(create_test_descriptor("Proc1"), create_test_factory()).unwrap();
        registry.register(create_test_descriptor("Proc2"), create_test_factory()).unwrap();
        registry.register_descriptor_only(create_test_descriptor("Proc3")).unwrap();

        let list = registry.list();
        assert_eq!(list.len(), 3);

        let names: Vec<String> = list.iter().map(|d| d.name.clone()).collect();
        assert!(names.contains(&"Proc1".to_string()));
        assert!(names.contains(&"Proc2".to_string()));
        assert!(names.contains(&"Proc3".to_string()));
    }

    #[test]
    fn test_list_by_tag() {
        let mut registry = ProcessorRegistry::new();

        let desc1 = ProcessorDescriptor::new("Proc1", "Description")
            .with_tags(vec!["source", "video"]);
        let desc2 = ProcessorDescriptor::new("Proc2", "Description")
            .with_tags(vec!["sink", "video"]);
        let desc3 = ProcessorDescriptor::new("Proc3", "Description")
            .with_tags(vec!["source", "audio"]);

        registry.register(desc1, create_test_factory()).unwrap();
        registry.register(desc2, create_test_factory()).unwrap();
        registry.register(desc3, create_test_factory()).unwrap();

        let sources = registry.list_by_tag("source");
        assert_eq!(sources.len(), 2);

        let video = registry.list_by_tag("video");
        assert_eq!(video.len(), 2);

        let audio = registry.list_by_tag("audio");
        assert_eq!(audio.len(), 1);
    }

    #[test]
    fn test_create_instance() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("TestProcessor");
        let factory = create_test_factory();

        registry.register(descriptor, factory).unwrap();

        let instance = registry.create_instance("TestProcessor");
        assert!(instance.is_ok());
    }

    #[test]
    fn test_create_instance_not_found() {
        let registry = ProcessorRegistry::new();

        let result = registry.create_instance("NonExistent");
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("not found"));
        }
    }

    #[test]
    fn test_create_instance_no_factory() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("ExternalProcessor");

        registry.register_descriptor_only(descriptor).unwrap();

        let result = registry.create_instance("ExternalProcessor");
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("no factory"));
        }
    }

    #[test]
    fn test_unregister() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("TestProcessor");

        registry.register(descriptor, create_test_factory()).unwrap();
        assert_eq!(registry.len(), 1);

        let removed = registry.unregister("TestProcessor");
        assert!(removed);
        assert_eq!(registry.len(), 0);

        let removed_again = registry.unregister("TestProcessor");
        assert!(!removed_again);
    }

    #[test]
    fn test_clear() {
        let mut registry = ProcessorRegistry::new();

        registry.register(create_test_descriptor("Proc1"), create_test_factory()).unwrap();
        registry.register(create_test_descriptor("Proc2"), create_test_factory()).unwrap();

        assert_eq!(registry.len(), 2);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn test_global_registry() {
        // Note: This test may interfere with other tests if run in parallel
        // In a real scenario, you'd want to reset the global state or use test isolation

        let descriptor = create_test_descriptor("GlobalTestProcessor");
        let factory = create_test_factory();

        register_processor(descriptor, factory).unwrap();

        assert!(is_processor_registered("GlobalTestProcessor"));

        let list = list_processors();
        assert!(list.iter().any(|d| d.name == "GlobalTestProcessor"));

        unregister_processor("GlobalTestProcessor");
    }
}
