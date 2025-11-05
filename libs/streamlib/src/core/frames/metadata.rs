//! Metadata value types for frame annotations
//!
//! Supports common metadata value types for flexibility across
//! video, audio, and data messages.

use std::collections::HashMap;

/// Metadata value types
///
/// Supports common metadata value types for flexibility.
#[derive(Debug, Clone)]
pub enum MetadataValue {
    /// String value
    String(String),
    /// Integer value
    Int(i64),
    /// Float value
    Float(f64),
    /// Boolean value
    Bool(bool),
    /// Nested metadata
    Map(HashMap<String, MetadataValue>),
    /// Array of values
    Array(Vec<MetadataValue>),
}

impl From<String> for MetadataValue {
    fn from(s: String) -> Self {
        MetadataValue::String(s)
    }
}

impl From<&str> for MetadataValue {
    fn from(s: &str) -> Self {
        MetadataValue::String(s.to_string())
    }
}

impl From<i64> for MetadataValue {
    fn from(i: i64) -> Self {
        MetadataValue::Int(i)
    }
}

impl From<f64> for MetadataValue {
    fn from(f: f64) -> Self {
        MetadataValue::Float(f)
    }
}

impl From<bool> for MetadataValue {
    fn from(b: bool) -> Self {
        MetadataValue::Bool(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_value_conversions() {
        let _str_val: MetadataValue = "test".into();
        let _int_val: MetadataValue = 42i64.into();
        let _float_val: MetadataValue = 2.71f64.into();
        let _bool_val: MetadataValue = true.into();
    }
}
