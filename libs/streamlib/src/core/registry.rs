//! Processor Registry
//!
//! Runtime registry for dynamic processor discovery and registration.
//! Supports both compile-time (via inventory) and runtime registration.

use super::{ProcessorDescriptor, StreamError};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use parking_lot::Mutex;

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

/// Register a processor type for auto-discovery
///
/// This macro handles all the boilerplate for inventory-based auto-registration.
/// Just call it once after your StreamProcessor implementation.
///
/// # Example
/// ```no_run
/// use streamlib::{StreamProcessor, ProcessorDescriptor, register_processor_type};
///
/// struct MyProcessor;
///
/// impl StreamProcessor for MyProcessor {
///     fn descriptor() -> Option<ProcessorDescriptor> {
///         Some(ProcessorDescriptor::new("MyProcessor", "Does cool things"))
///     }
///     // ... other trait methods
/// }
///
/// // That's it! One line to auto-register
/// register_processor_type!(MyProcessor);
/// ```
#[macro_export]
macro_rules! register_processor_type {
    ($processor_type:ty) => {
        // Create a hidden descriptor provider
        const _: () = {
            struct __DescriptorProvider;

            impl $crate::DescriptorProvider for __DescriptorProvider {
                fn descriptor(&self) -> $crate::ProcessorDescriptor {
                    // Call the static descriptor() method from StreamSource/StreamSink/StreamTransform
                    // (not the instance method from StreamElement)
                    <$processor_type as $crate::core::traits::StreamSource>::descriptor().expect(concat!(
                        stringify!($processor_type),
                        " must provide a descriptor"
                    ))
                }
            }

            // Auto-register at compile time
            inventory::submit! {
                &__DescriptorProvider as &dyn $crate::DescriptorProvider
            }
        };
    };
}

/// Entry in the processor registry (descriptor-only for AI agent documentation)
#[derive(Clone)]
pub struct ProcessorRegistration {
    /// Processor metadata
    pub descriptor: ProcessorDescriptor,
}

impl ProcessorRegistration {
    /// Create a new registration
    pub fn new(descriptor: ProcessorDescriptor) -> Self {
        Self { descriptor }
    }
}

/// Processor registry for AI agent discovery
///
/// This is pure documentation for AI agents to understand what processors exist
/// and how to use them. It does NOT create processor instances - that's handled
/// directly in MCP tools.
///
/// Purpose: Help AI agents understand:
/// - What processors are available
/// - What inputs/outputs they have
/// - How to configure them
/// - When to use them
pub struct ProcessorRegistry {
    /// Registered processor descriptors by name
    processors: HashMap<String, ProcessorRegistration>,
}

impl ProcessorRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            processors: HashMap::new(),
        }
    }

    /// Register a processor descriptor for AI agent discovery
    ///
    /// This is pure documentation - it does NOT enable instantiation.
    /// Processor creation is handled directly in MCP tools.
    ///
    /// # Arguments
    /// * `descriptor` - Processor metadata
    ///
    /// # Returns
    /// Error if a processor with the same name is already registered
    pub fn register(&mut self, descriptor: ProcessorDescriptor) -> crate::Result<()> {
        let name = descriptor.name.clone();

        if self.processors.contains_key(&name) {
            return Err(StreamError::Configuration(format!(
                "Processor '{}' is already registered",
                name
            )));
        }

        self.processors
            .insert(name, ProcessorRegistration::new(descriptor));

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
            // This is pure documentation for AI agents
            for provider in inventory::iter::<&dyn DescriptorProvider> {
                let descriptor = provider.descriptor();
                let name = descriptor.name.clone();

                if let Err(e) = registry.register(descriptor) {
                    // Log warning but don't fail - allow duplicate submissions to be gracefully ignored
                    tracing::warn!("Failed to auto-register processor '{}': {}", name, e);
                }
            }

            tracing::info!(
                "Auto-registered {} processor descriptors for AI agent discovery",
                registry.len()
            );

            // Create Arc after all registrations are complete
            Arc::new(Mutex::new(registry))
        })
        .clone()
}

/// Register a processor descriptor in the global registry
///
/// This is for AI agent documentation only - it does not enable instantiation.
/// Processor creation is handled directly in MCP tools based on processor type.
///
/// # Arguments
/// * `descriptor` - Processor metadata
///
/// # Example
/// ```no_run
/// use streamlib_core::{register_processor, ProcessorDescriptor};
///
/// let descriptor = ProcessorDescriptor::new("MyProcessor", "Does cool stuff");
/// register_processor(descriptor).unwrap();
/// ```
pub fn register_processor(descriptor: ProcessorDescriptor) -> crate::Result<()> {
    global_registry().lock().register(descriptor)
}

/// List all registered processors
pub fn list_processors() -> Vec<ProcessorDescriptor> {
    global_registry().lock().list()
}

/// List processors filtered by tag
pub fn list_processors_by_tag(tag: &str) -> Vec<ProcessorDescriptor> {
    global_registry().lock().list_by_tag(tag)
}

/// Check if a processor is registered
pub fn is_processor_registered(name: &str) -> bool {
    global_registry().lock().contains(name)
}

/// Unregister a processor
pub fn unregister_processor(name: &str) -> bool {
    global_registry().lock().unregister(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ProcessorDescriptor;

    fn create_test_descriptor(name: &str) -> ProcessorDescriptor {
        ProcessorDescriptor::new(name, &format!("{} description", name))
            .with_tags(vec!["test", "mock"])
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

        registry.register(descriptor.clone()).unwrap();

        assert_eq!(registry.len(), 1);
        assert!(registry.contains("TestProcessor"));

        let registration = registry.get("TestProcessor").unwrap();
        assert_eq!(registration.descriptor.name, "TestProcessor");
    }

    #[test]
    fn test_register_duplicate() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("TestProcessor");

        registry.register(descriptor.clone()).unwrap();

        // Try to register again
        let result = registry.register(descriptor);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("already registered"));
    }

    #[test]
    fn test_list_processors() {
        let mut registry = ProcessorRegistry::new();

        registry.register(create_test_descriptor("Proc1")).unwrap();
        registry.register(create_test_descriptor("Proc2")).unwrap();
        registry.register(create_test_descriptor("Proc3")).unwrap();

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

        let desc1 =
            ProcessorDescriptor::new("Proc1", "Description").with_tags(vec!["source", "video"]);
        let desc2 =
            ProcessorDescriptor::new("Proc2", "Description").with_tags(vec!["sink", "video"]);
        let desc3 =
            ProcessorDescriptor::new("Proc3", "Description").with_tags(vec!["source", "audio"]);

        registry.register(desc1).unwrap();
        registry.register(desc2).unwrap();
        registry.register(desc3).unwrap();

        let sources = registry.list_by_tag("source");
        assert_eq!(sources.len(), 2);

        let video = registry.list_by_tag("video");
        assert_eq!(video.len(), 2);

        let audio = registry.list_by_tag("audio");
        assert_eq!(audio.len(), 1);
    }

    #[test]
    fn test_unregister() {
        let mut registry = ProcessorRegistry::new();
        let descriptor = create_test_descriptor("TestProcessor");

        registry.register(descriptor).unwrap();
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

        registry.register(create_test_descriptor("Proc1")).unwrap();
        registry.register(create_test_descriptor("Proc2")).unwrap();

        assert_eq!(registry.len(), 2);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn test_global_registry() {
        // Note: This test may interfere with other tests if run in parallel
        // In a real scenario, you'd want to reset the global state or use test isolation

        let descriptor = create_test_descriptor("GlobalTestProcessor");

        register_processor(descriptor).unwrap();

        assert!(is_processor_registered("GlobalTestProcessor"));

        let list = list_processors();
        assert!(list.iter().any(|d| d.name == "GlobalTestProcessor"));

        unregister_processor("GlobalTestProcessor");
    }
}
