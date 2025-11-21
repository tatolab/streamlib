use super::{ProcessorDescriptor, StreamError};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub trait DescriptorProvider: Sync {
    fn descriptor(&self) -> ProcessorDescriptor;
}

inventory::collect!(&'static dyn DescriptorProvider);

#[macro_export]
macro_rules! register_processor_type {
    ($processor_type:ty) => {
        const _: () = {
            struct __DescriptorProvider;

            impl $crate::DescriptorProvider for __DescriptorProvider {
                fn descriptor(&self) -> $crate::ProcessorDescriptor {
                    <$processor_type as $crate::core::traits::StreamProcessor>::descriptor().expect(
                        concat!(stringify!($processor_type), " must provide a descriptor"),
                    )
                }
            }

            inventory::submit! {
                &__DescriptorProvider as &dyn $crate::DescriptorProvider
            }
        };
    };
}

#[derive(Clone)]
pub struct ProcessorRegistration {
    pub descriptor: ProcessorDescriptor,
}

impl ProcessorRegistration {
    pub fn new(descriptor: ProcessorDescriptor) -> Self {
        Self { descriptor }
    }
}

pub struct ProcessorRegistry {
    processors: HashMap<String, ProcessorRegistration>,
}

impl ProcessorRegistry {
    pub fn new() -> Self {
        Self {
            processors: HashMap::new(),
        }
    }

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

    pub fn get(&self, name: &str) -> Option<&ProcessorRegistration> {
        self.processors.get(name)
    }

    pub fn list(&self) -> Vec<ProcessorDescriptor> {
        self.processors
            .values()
            .map(|reg| reg.descriptor.clone())
            .collect()
    }

    pub fn list_by_tag(&self, tag: &str) -> Vec<ProcessorDescriptor> {
        self.processors
            .values()
            .filter(|reg| reg.descriptor.tags.iter().any(|t| t == tag))
            .map(|reg| reg.descriptor.clone())
            .collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.processors.contains_key(name)
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        self.processors.remove(name).is_some()
    }

    pub fn clear(&mut self) {
        self.processors.clear();
    }

    pub fn len(&self) -> usize {
        self.processors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }
}

impl Default for ProcessorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_REGISTRY: OnceLock<Arc<Mutex<ProcessorRegistry>>> = OnceLock::new();

pub fn global_registry() -> Arc<Mutex<ProcessorRegistry>> {
    GLOBAL_REGISTRY
        .get_or_init(|| {
            let mut registry = ProcessorRegistry::new();

            for provider in inventory::iter::<&dyn DescriptorProvider> {
                let descriptor = provider.descriptor();
                let name = descriptor.name.clone();

                if let Err(e) = registry.register(descriptor) {
                    tracing::warn!("Failed to auto-register processor '{}': {}", name, e);
                }
            }

            tracing::info!(
                "Auto-registered {} processor descriptors for AI agent discovery",
                registry.len()
            );

            Arc::new(Mutex::new(registry))
        })
        .clone()
}

pub fn register_processor(descriptor: ProcessorDescriptor) -> crate::Result<()> {
    global_registry().lock().register(descriptor)
}

pub fn list_processors() -> Vec<ProcessorDescriptor> {
    global_registry().lock().list()
}

pub fn list_processors_by_tag(tag: &str) -> Vec<ProcessorDescriptor> {
    global_registry().lock().list_by_tag(tag)
}

pub fn is_processor_registered(name: &str) -> bool {
    global_registry().lock().contains(name)
}

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
        let descriptor = create_test_descriptor("GlobalTestProcessor");

        register_processor(descriptor).unwrap();

        assert!(is_processor_registered("GlobalTestProcessor"));

        let list = list_processors();
        assert!(list.iter().any(|d| d.name == "GlobalTestProcessor"));

        unregister_processor("GlobalTestProcessor");
    }
}
