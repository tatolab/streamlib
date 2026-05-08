// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Global cache for format converters.
//!
//! Converters are expensive to create (vImageConverter allocation) but cheap
//! to use concurrently. This cache ensures each (source, dest) format pair
//! has exactly one converter shared across all processors.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use super::{PixelFormat, RhiFormatConverter};
use crate::core::Result;

/// Key for the converter cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FormatConverterKey {
    source: PixelFormat,
    dest: PixelFormat,
}

/// Global cache state.
struct FormatConverterCacheInner {
    converters: RwLock<HashMap<FormatConverterKey, Arc<RhiFormatConverter>>>,
}

/// Global converter cache instance.
static CACHE: OnceLock<FormatConverterCacheInner> = OnceLock::new();

fn get_cache() -> &'static FormatConverterCacheInner {
    CACHE.get_or_init(|| FormatConverterCacheInner {
        converters: RwLock::new(HashMap::new()),
    })
}

/// Global cache for format converters.
///
/// Provides shared converters across all processors. Each converter is created
/// once and reused for all subsequent requests with the same format pair.
///
/// Thread-safe: converters can be used concurrently without locking. Only
/// converter creation (which happens rarely) requires synchronization.
pub struct RhiFormatConverterCache;

impl RhiFormatConverterCache {
    /// Get or create a converter for the given format pair.
    ///
    /// Returns an Arc-wrapped converter that can be shared across threads.
    /// The converter is created lazily on first request and cached for
    /// subsequent requests.
    pub fn get(source: PixelFormat, dest: PixelFormat) -> Result<Arc<RhiFormatConverter>> {
        let cache = get_cache();
        let key = FormatConverterKey { source, dest };

        // Fast path: check if converter exists (read lock)
        {
            let converters = cache.converters.read().unwrap();
            if let Some(converter) = converters.get(&key) {
                return Ok(Arc::clone(converter));
            }
        }

        // Slow path: create converter (write lock)
        let mut converters = cache.converters.write().unwrap();

        // Double-check after acquiring write lock
        if let Some(converter) = converters.get(&key) {
            return Ok(Arc::clone(converter));
        }

        // Create new converter
        let converter = Arc::new(RhiFormatConverter::new(source, dest)?);
        converters.insert(key, Arc::clone(&converter));

        tracing::debug!("Created format converter: {:?} -> {:?}", source, dest);

        Ok(converter)
    }

    /// Check if a converter exists in the cache.
    pub fn contains(source: PixelFormat, dest: PixelFormat) -> bool {
        let cache = get_cache();
        let key = FormatConverterKey { source, dest };
        let converters = cache.converters.read().unwrap();
        converters.contains_key(&key)
    }

    /// Get the number of cached converters.
    pub fn len() -> usize {
        let cache = get_cache();
        let converters = cache.converters.read().unwrap();
        converters.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty() -> bool {
        Self::len() == 0
    }
}
