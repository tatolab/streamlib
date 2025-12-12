// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use crate::core::error::{Result, StreamError};
use crate::core::graph::{LinkDirection, ProcessorUniqueId};
use std::fmt;

/// Reference to a port on a processor node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPortRef {
    pub processor_id: ProcessorUniqueId,
    pub port_name: String,
    pub direction: LinkDirection,
}

impl LinkPortRef {
    pub fn output(
        processor_id: impl Into<ProcessorUniqueId>,
        port_name: impl Into<String>,
    ) -> Option<Self> {
        Self {
            processor_id: processor_id.into(),
            port_name: port_name.into(),
            direction: LinkDirection::Output,
        }
    }

    pub fn input(processor_id: impl Into<ProcessorUniqueId>, port_name: impl Into<String>) -> Self {
        Self {
            processor_id: processor_id.into(),
            port_name: port_name.into(),
            direction: LinkDirection::Input,
        }
    }

    /// Alias for [`Self::output`] - creates a source endpoint for link construction.
    pub fn source(
        processor_id: impl Into<ProcessorUniqueId>,
        port_name: impl Into<String>,
    ) -> Self {
        Self::output(processor_id, port_name)
    }

    /// Alias for [`Self::input`] - creates a target endpoint for link construction.
    pub fn target(
        processor_id: impl Into<ProcessorUniqueId>,
        port_name: impl Into<String>,
    ) -> Self {
        Self::input(processor_id, port_name)
    }

    pub fn is_output(&self) -> bool {
        self.direction == LinkDirection::Output
    }

    pub fn is_input(&self) -> bool {
        self.direction == LinkDirection::Input
    }

    /// Parse "processor_id.port_name" format.
    pub fn parse(address: &str, direction: LinkDirection) -> Result<Self> {
        let parts: Vec<&str> = address.splitn(2, '.').collect();
        if parts.len() != 2 {
            return Err(StreamError::InvalidPortAddress(format!(
                "Invalid port address '{}'. Expected format: 'processor_id.port_name'",
                address
            )));
        }

        let processor_id = parts[0];
        let port_name = parts[1];

        if processor_id.is_empty() {
            return Err(StreamError::InvalidPortAddress(format!(
                "Empty processor ID in port address '{}'",
                address
            )));
        }

        if port_name.is_empty() {
            return Err(StreamError::InvalidPortAddress(format!(
                "Empty port name in port address '{}'",
                address
            )));
        }

        Ok(Self {
            processor_id: processor_id.into(),
            port_name: port_name.to_string(),
            direction,
        })
    }

    pub fn parse_output(address: &str) -> Result<Self> {
        Self::parse(address, LinkDirection::Output)
    }

    pub fn parse_input(address: &str) -> Result<Self> {
        Self::parse(address, LinkDirection::Input)
    }
}

impl std::fmt::Display for LinkPortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Use write! macro to format the output into the formatter 'f'
        write!(f, "{}.{}", self.processor_id, self.port_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_port_ref_output() {
        let port = LinkPortRef::output("camera_0", "video");
        assert_eq!(port.processor_id, "camera_0");
        assert_eq!(port.port_name, "video");
        assert!(port.is_output());
        assert!(!port.is_input());
        assert_eq!(
            format!("{}.{}", port.processor_id, port.port_name),
            "camera_0.video"
        );
    }

    #[test]
    fn test_link_port_ref_input() {
        let port = LinkPortRef::input("display_0", "video");
        assert_eq!(port.processor_id, "display_0");
        assert_eq!(port.port_name, "video");
        assert!(port.is_input());
        assert!(!port.is_output());
        assert_eq!(
            format!("{}.{}", port.processor_id, port.port_name),
            "display_0.video"
        );
    }

    #[test]
    fn test_parse_output() {
        let port = LinkPortRef::parse_output("camera_0.main_video").unwrap();
        assert_eq!(port.processor_id, "camera_0");
        assert_eq!(port.port_name, "main_video");
        assert!(port.is_output());
    }

    #[test]
    fn test_parse_input() {
        let port = LinkPortRef::parse_input("mixer_0.audio_left").unwrap();
        assert_eq!(port.processor_id, "mixer_0");
        assert_eq!(port.port_name, "audio_left");
        assert!(port.is_input());
    }

    #[test]
    fn test_parse_invalid_no_dot() {
        let result = LinkPortRef::parse("camera_0_video", LinkDirection::Output);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_empty_processor() {
        let result = LinkPortRef::parse(".video", LinkDirection::Output);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_empty_port() {
        let result = LinkPortRef::parse("camera_0.", LinkDirection::Output);
        assert!(result.is_err());
    }

    #[test]
    fn test_into_link_port_ref_from_str() {
        let port: LinkPortRef = "source_0.thumbnail"
            .into_link_port_ref(LinkDirection::Output)
            .unwrap();
        assert_eq!(port.processor_id, "source_0");
        assert_eq!(port.port_name, "thumbnail");
        assert!(port.is_output());
    }

    #[test]
    fn test_into_link_port_ref_preserves_direction() {
        let original = LinkPortRef::output("cam_0", "video");
        // Even if we pass Input direction, LinkPortRef keeps its own direction
        let converted = original.into_link_port_ref(LinkDirection::Input).unwrap();
        assert!(converted.is_output());
    }
}
