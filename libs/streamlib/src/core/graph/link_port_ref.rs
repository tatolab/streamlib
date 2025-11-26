//! Link port references for type-safe connection API
//!
//! This module provides `LinkPortRef` for building links in a type-safe way.
//! These types are NOT serializable - they exist only for the runtime API.
//!
//! # Creating LinkPortRefs
//!
//! There are multiple ways to create a `LinkPortRef`:
//!
//! ```ignore
//! // 1. From ProcessorNode (validates port exists)
//! let port = camera_node.output("video");
//!
//! // 2. From address string (parsed, direction inferred by connect())
//! let port = LinkPortRef::parse("camera_0.video", LinkDirection::Output)?;
//!
//! // 3. From marker types (compile-time validation) - see output::<T>() helper
//! let port = output::<CameraProcessor::outputs::video>(&camera_node);
//! ```

use super::link::LinkDirection;
use super::ProcessorId;
use crate::core::error::{Result, StreamError};

/// Reference to a port on a processor node for creating links
///
/// This is a lightweight reference used for the `connect()` API.
/// It encodes the processor ID, port name, and direction.
///
/// Created by `ProcessorNode::output()` and `ProcessorNode::input()`.
///
/// NOT serializable - this is a runtime-only type for API ergonomics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPortRef {
    /// The processor this port belongs to
    pub processor_id: ProcessorId,
    /// The name of the port
    pub port_name: String,
    /// Whether this is an input or output port
    pub direction: LinkDirection,
}

impl LinkPortRef {
    /// Create a new output port reference
    pub fn output(processor_id: ProcessorId, port_name: impl Into<String>) -> Self {
        Self {
            processor_id,
            port_name: port_name.into(),
            direction: LinkDirection::Output,
        }
    }

    /// Create a new input port reference
    pub fn input(processor_id: ProcessorId, port_name: impl Into<String>) -> Self {
        Self {
            processor_id,
            port_name: port_name.into(),
            direction: LinkDirection::Input,
        }
    }

    /// Convert to port address string (processor_id.port_name)
    pub fn to_address(&self) -> String {
        format!("{}.{}", self.processor_id, self.port_name)
    }

    /// Check if this is an output port
    pub fn is_output(&self) -> bool {
        self.direction == LinkDirection::Output
    }

    /// Check if this is an input port
    pub fn is_input(&self) -> bool {
        self.direction == LinkDirection::Input
    }

    /// Parse a port address string into a LinkPortRef
    ///
    /// Format: "processor_id.port_name"
    ///
    /// The direction must be provided since it cannot be inferred from the string.
    ///
    /// # Example
    /// ```ignore
    /// let port = LinkPortRef::parse("camera_0.video", LinkDirection::Output)?;
    /// ```
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
            processor_id: processor_id.to_string(),
            port_name: port_name.to_string(),
            direction,
        })
    }

    /// Parse an output port address
    pub fn parse_output(address: &str) -> Result<Self> {
        Self::parse(address, LinkDirection::Output)
    }

    /// Parse an input port address
    pub fn parse_input(address: &str) -> Result<Self> {
        Self::parse(address, LinkDirection::Input)
    }
}

/// Trait for types that can be converted into a LinkPortRef
///
/// This enables `connect()` to accept multiple types:
/// - `LinkPortRef` directly
/// - `&str` or `String` address (requires direction context)
/// - Marker types from the macro
pub trait IntoLinkPortRef {
    /// Convert into a LinkPortRef with the given direction
    fn into_link_port_ref(self, direction: LinkDirection) -> Result<LinkPortRef>;
}

impl IntoLinkPortRef for LinkPortRef {
    fn into_link_port_ref(self, _direction: LinkDirection) -> Result<LinkPortRef> {
        // LinkPortRef already has direction, use its own
        Ok(self)
    }
}

impl IntoLinkPortRef for &str {
    fn into_link_port_ref(self, direction: LinkDirection) -> Result<LinkPortRef> {
        LinkPortRef::parse(self, direction)
    }
}

impl IntoLinkPortRef for String {
    fn into_link_port_ref(self, direction: LinkDirection) -> Result<LinkPortRef> {
        LinkPortRef::parse(&self, direction)
    }
}

impl IntoLinkPortRef for &String {
    fn into_link_port_ref(self, direction: LinkDirection) -> Result<LinkPortRef> {
        LinkPortRef::parse(self, direction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_port_ref_output() {
        let port = LinkPortRef::output("camera_0".to_string(), "video");
        assert_eq!(port.processor_id, "camera_0");
        assert_eq!(port.port_name, "video");
        assert!(port.is_output());
        assert!(!port.is_input());
        assert_eq!(port.to_address(), "camera_0.video");
    }

    #[test]
    fn test_link_port_ref_input() {
        let port = LinkPortRef::input("display_0".to_string(), "video");
        assert_eq!(port.processor_id, "display_0");
        assert_eq!(port.port_name, "video");
        assert!(port.is_input());
        assert!(!port.is_output());
        assert_eq!(port.to_address(), "display_0.video");
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
        let original = LinkPortRef::output("cam_0".to_string(), "video");
        // Even if we pass Input direction, LinkPortRef keeps its own direction
        let converted = original.into_link_port_ref(LinkDirection::Input).unwrap();
        assert!(converted.is_output());
    }
}
