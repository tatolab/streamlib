// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI backend selection with runtime resolution.
//!
//! The backend can be selected at runtime via:
//! 1. Explicit parameter passed to `RhiBackend::resolve()`
//! 2. `STREAMLIB_RHI_BACKEND` environment variable
//! 3. Platform default (Metal on macOS, Vulkan on Linux)

use std::str::FromStr;

/// RHI backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RhiBackend {
    /// Metal backend (macOS/iOS default).
    Metal,
    /// Vulkan backend (Linux default, cross-platform).
    Vulkan,
    /// OpenGL backend (for Skia/interop requirements).
    OpenGL,
}

impl RhiBackend {
    /// Environment variable name for backend override.
    pub const ENV_VAR: &'static str = "STREAMLIB_RHI_BACKEND";

    /// Resolve the backend to use.
    ///
    /// Resolution priority:
    /// 1. Explicit value (if provided)
    /// 2. `STREAMLIB_RHI_BACKEND` environment variable
    /// 3. Platform default
    pub fn resolve(explicit: Option<Self>) -> Self {
        if let Some(backend) = explicit {
            return backend;
        }

        if let Ok(env_value) = std::env::var(Self::ENV_VAR) {
            if let Ok(backend) = env_value.parse() {
                return backend;
            }
        }

        Self::platform_default()
    }

    /// Get the platform default backend.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn platform_default() -> Self {
        Self::Metal
    }

    /// Get the platform default backend.
    #[cfg(target_os = "linux")]
    pub fn platform_default() -> Self {
        Self::Vulkan
    }

    /// Get the platform default backend.
    #[cfg(target_os = "windows")]
    pub fn platform_default() -> Self {
        Self::Vulkan
    }

    /// Get the platform default backend (fallback).
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "windows"
    )))]
    pub fn platform_default() -> Self {
        Self::OpenGL
    }

    /// Check if this backend is available on the current platform.
    pub fn is_available(&self) -> bool {
        match self {
            Self::Metal => cfg!(any(target_os = "macos", target_os = "ios")),
            Self::Vulkan => cfg!(any(
                target_os = "linux",
                target_os = "windows",
                target_os = "macos"
            )),
            Self::OpenGL => true, // OpenGL available on all platforms
        }
    }

    /// Get the backend name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metal => "metal",
            Self::Vulkan => "vulkan",
            Self::OpenGL => "opengl",
        }
    }
}

impl FromStr for RhiBackend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "metal" => Ok(Self::Metal),
            "vulkan" => Ok(Self::Vulkan),
            "opengl" | "gl" => Ok(Self::OpenGL),
            _ => Err(format!(
                "Unknown backend '{}'. Valid values: metal, vulkan, opengl",
                s
            )),
        }
    }
}

impl std::fmt::Display for RhiBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_backend() {
        assert_eq!("metal".parse::<RhiBackend>().unwrap(), RhiBackend::Metal);
        assert_eq!("Metal".parse::<RhiBackend>().unwrap(), RhiBackend::Metal);
        assert_eq!("vulkan".parse::<RhiBackend>().unwrap(), RhiBackend::Vulkan);
        assert_eq!("opengl".parse::<RhiBackend>().unwrap(), RhiBackend::OpenGL);
        assert_eq!("gl".parse::<RhiBackend>().unwrap(), RhiBackend::OpenGL);
        assert!("invalid".parse::<RhiBackend>().is_err());
    }

    #[test]
    fn test_resolve_explicit() {
        assert_eq!(
            RhiBackend::resolve(Some(RhiBackend::OpenGL)),
            RhiBackend::OpenGL
        );
    }

    #[test]
    fn test_display() {
        assert_eq!(RhiBackend::Metal.to_string(), "metal");
        assert_eq!(RhiBackend::Vulkan.to_string(), "vulkan");
        assert_eq!(RhiBackend::OpenGL.to_string(), "opengl");
    }
}
