// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ops::Deref;

/// Internal APIs - DO NOT USE directly.
pub mod __private {
    use super::LinkId;

    /// Creates a [`LinkId`] without validation. Use [`LinkId::from_string`] instead.
    pub fn new_unchecked(id: impl Into<String>) -> LinkId {
        let s = id.into();

        // Debug-only validation to catch internal misuse during development
        debug_assert!(!s.is_empty(), "LinkId cannot be empty (got: {:?})", s);
        debug_assert!(
            s.chars().all(|c| {
                c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '>' || c == ':'
            }),
            "LinkId '{}' contains invalid characters. Only alphanumeric, '_', '-', '.', '>', ':' allowed",
            s
        );

        LinkId(s)
    }
}

/// A validated, unique link identifier.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct LinkId(String);

/// Errors that can occur when parsing a [`LinkId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkIdError {
    Empty,
    InvalidCharacters(String),
    InvalidFormat(String),
}

impl std::fmt::Display for LinkIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "Link ID cannot be empty"),
            Self::InvalidCharacters(id) => {
                write!(f, "Link ID '{}' contains invalid characters", id)
            }
            Self::InvalidFormat(msg) => write!(f, "Invalid link ID format: {}", msg),
        }
    }
}

impl std::error::Error for LinkIdError {}

impl LinkId {
    /// Parse and validate a link ID from a string.
    pub fn from_string(s: impl Into<String>) -> Result<Self, LinkIdError> {
        let s = s.into();

        if s.is_empty() {
            return Err(LinkIdError::Empty);
        }

        // Validate format: alphanumeric, underscore, hyphen, dot, arrow
        if !s.chars().all(|c| {
            c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '>' || c == ':'
        }) {
            return Err(LinkIdError::InvalidCharacters(s));
        }

        Ok(Self(s))
    }

    /// Get the string representation
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the LinkId and get the inner String
    #[inline]
    pub fn into_inner(self) -> String {
        self.0
    }
}

// Allow using &LinkId where &str is expected
impl Deref for LinkId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for LinkId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_link_ids() {
        assert!(LinkId::from_string("simple").is_ok());
        assert!(LinkId::from_string("with_underscore").is_ok());
        assert!(LinkId::from_string("with-hyphen").is_ok());
        assert!(LinkId::from_string("with.dot").is_ok());
        assert!(LinkId::from_string("proc1->proc2").is_ok());
        assert!(LinkId::from_string("complex_id-123.v2").is_ok());
    }

    #[test]
    fn test_invalid_link_ids() {
        assert!(matches!(LinkId::from_string(""), Err(LinkIdError::Empty)));
        assert!(matches!(
            LinkId::from_string("has spaces"),
            Err(LinkIdError::InvalidCharacters(_))
        ));
        assert!(matches!(
            LinkId::from_string("has@symbol"),
            Err(LinkIdError::InvalidCharacters(_))
        ));
        assert!(matches!(
            LinkId::from_string("has/slash"),
            Err(LinkIdError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn test_deref_to_str() {
        let id = __private::new_unchecked("test_id");
        let s: &str = &id; // Should deref automatically
        assert_eq!(s, "test_id");
    }

    #[test]
    fn test_as_str() {
        let id = __private::new_unchecked("test_id");
        assert_eq!(id.as_str(), "test_id");
    }

    #[test]
    fn test_into_inner() {
        let id = __private::new_unchecked("test_id");
        let s = id.into_inner();
        assert_eq!(s, "test_id");
    }

    #[test]
    fn test_comparison() {
        let id1 = __private::new_unchecked("test");
        let id2 = __private::new_unchecked("test");
        let id3 = __private::new_unchecked("other");

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }
}
