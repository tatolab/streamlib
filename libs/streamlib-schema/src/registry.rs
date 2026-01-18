// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema registry for local caching and remote fetching.

use crate::definition::SchemaDefinition;
use crate::error::{Result, SchemaError};
use crate::parser::parse_yaml;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Local schema registry with caching.
pub struct SchemaRegistry {
    /// Cached schemas by full name.
    cache: HashMap<String, Arc<SchemaDefinition>>,

    /// Local cache directory.
    cache_dir: PathBuf,

    /// Remote registry URL.
    registry_url: String,
}

impl SchemaRegistry {
    /// Create a new schema registry with default paths.
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| {
                SchemaError::IoError(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not determine cache directory",
                ))
            })?
            .join("streamlib")
            .join("schemas");

        std::fs::create_dir_all(&cache_dir)?;

        Ok(Self {
            cache: HashMap::new(),
            cache_dir,
            registry_url: "https://schemas.streamlib.dev".to_string(),
        })
    }

    /// Create a registry with custom paths.
    pub fn with_config(cache_dir: PathBuf, registry_url: String) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir)?;

        Ok(Self {
            cache: HashMap::new(),
            cache_dir,
            registry_url,
        })
    }

    /// Set the remote registry URL.
    pub fn set_registry_url(&mut self, url: &str) {
        self.registry_url = url.to_string();
    }

    /// Get a schema by full name (e.g., "com.tatolab.videoframe@1.0.0").
    ///
    /// Checks in-memory cache first, then local disk cache.
    /// Does NOT fetch from remote - use `fetch` for that.
    pub fn get(&self, full_name: &str) -> Option<Arc<SchemaDefinition>> {
        // Check in-memory cache
        if let Some(schema) = self.cache.get(full_name) {
            return Some(schema.clone());
        }

        // Check disk cache
        let cache_path = self.cache_path(full_name);
        if cache_path.exists() {
            if let Ok(schema) = crate::parser::parse_yaml_file(&cache_path) {
                return Some(Arc::new(schema));
            }
        }

        None
    }

    /// Resolve a schema by full name, fetching from remote if needed.
    ///
    /// This is the main entry point for getting schemas.
    pub fn resolve(&mut self, full_name: &str) -> Result<Arc<SchemaDefinition>> {
        // Check in-memory cache
        if let Some(schema) = self.cache.get(full_name) {
            return Ok(schema.clone());
        }

        // Check disk cache
        let cache_path = self.cache_path(full_name);
        if cache_path.exists() {
            let schema = Arc::new(crate::parser::parse_yaml_file(&cache_path)?);
            self.cache.insert(full_name.to_string(), schema.clone());
            return Ok(schema);
        }

        // Would need to fetch from remote
        Err(SchemaError::NotFound {
            name: full_name.to_string(),
        })
    }

    /// Register a schema from a local YAML file.
    pub fn register_local(&mut self, path: &Path) -> Result<Arc<SchemaDefinition>> {
        let schema = Arc::new(crate::parser::parse_yaml_file(path)?);
        let full_name = schema.full_name();

        // Save to disk cache
        let cache_path = self.cache_path(&full_name);
        std::fs::copy(path, &cache_path)?;

        // Add to in-memory cache
        self.cache.insert(full_name, schema.clone());

        Ok(schema)
    }

    /// Register a schema from YAML content.
    pub fn register_yaml(&mut self, yaml: &str) -> Result<Arc<SchemaDefinition>> {
        let schema = Arc::new(parse_yaml(yaml)?);
        let full_name = schema.full_name();

        // Save to disk cache
        let cache_path = self.cache_path(&full_name);
        std::fs::write(&cache_path, yaml)?;

        // Add to in-memory cache
        self.cache.insert(full_name, schema.clone());

        Ok(schema)
    }

    /// Get the cache file path for a schema.
    fn cache_path(&self, full_name: &str) -> PathBuf {
        // Replace @ with _v for filename safety
        let filename = full_name.replace('@', "_v") + ".yaml";
        self.cache_dir.join(filename)
    }

    /// List all cached schemas.
    pub fn list_cached(&self) -> Result<Vec<String>> {
        let mut schemas = Vec::new();

        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "yaml").unwrap_or(false) {
                if let Ok(schema) = crate::parser::parse_yaml_file(&path) {
                    schemas.push(schema.full_name());
                }
            }
        }

        Ok(schemas)
    }

    /// Clear the in-memory cache.
    pub fn clear_memory_cache(&mut self) {
        self.cache.clear();
    }

    /// Clear the disk cache.
    pub fn clear_disk_cache(&self) -> Result<()> {
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            std::fs::remove_file(entry.path())?;
        }
        Ok(())
    }

    /// Get the cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Get the registry URL.
    pub fn registry_url(&self) -> &str {
        &self.registry_url
    }
}

impl Default for SchemaRegistry {
    fn default() -> Self {
        Self::new().expect("Failed to create default schema registry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_registry() -> (SchemaRegistry, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let registry = SchemaRegistry::with_config(
            temp_dir.path().to_path_buf(),
            "https://test.example.com".to_string(),
        )
        .unwrap();
        (registry, temp_dir)
    }

    #[test]
    fn test_register_yaml() {
        let (mut registry, _temp) = test_registry();

        let yaml = r#"
name: com.test.example
version: 1.0.0
fields:
  - name: value
    type: string
"#;

        let schema = registry.register_yaml(yaml).unwrap();
        assert_eq!(schema.name, "com.test.example");
        assert_eq!(schema.version, "1.0.0");

        // Should be retrievable
        let retrieved = registry.resolve("com.test.example@1.0.0").unwrap();
        assert_eq!(retrieved.name, "com.test.example");
    }

    #[test]
    fn test_cache_persistence() {
        let temp_dir = TempDir::new().unwrap();

        // Register in first registry
        {
            let mut registry = SchemaRegistry::with_config(
                temp_dir.path().to_path_buf(),
                "https://test.example.com".to_string(),
            )
            .unwrap();

            let yaml = r#"
name: com.test.persistent
version: 2.0.0
fields:
  - name: data
    type: bytes
"#;
            registry.register_yaml(yaml).unwrap();
        }

        // Should be loadable from new registry instance
        {
            let mut registry = SchemaRegistry::with_config(
                temp_dir.path().to_path_buf(),
                "https://test.example.com".to_string(),
            )
            .unwrap();

            let schema = registry.resolve("com.test.persistent@2.0.0").unwrap();
            assert_eq!(schema.name, "com.test.persistent");
        }
    }

    #[test]
    fn test_not_found() {
        let (mut registry, _temp) = test_registry();

        let result = registry.resolve("com.nonexistent.schema@1.0.0");
        assert!(matches!(result, Err(SchemaError::NotFound { .. })));
    }
}
