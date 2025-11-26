//! Link identifier with validation
//!
//! Provides a type-safe wrapper for link identifiers that enforces validation
//! and prevents mixing up link IDs with other string types (processor IDs, port names, etc.).

use std::ops::Deref;

/// **Internal APIs - DO NOT USE**
///
/// This module contains internal implementation details that bypass safety checks.
/// These functions are used exclusively by macro-generated code and runtime internals.
///
/// # For Library Users
///
/// **If you're reading this in the source code**: These APIs are not part of the public
/// contract and may change or be removed without notice. Use the public APIs instead.
///
/// # For Contributors/Forkers
///
/// If you're tempted to use these functions directly: **don't**. They exist to optimize
/// hot paths where we've already guaranteed validity. Using them incorrectly will
/// introduce subtle bugs and panics in debug builds.
pub mod __private {
    use super::LinkId;

    /// **INTERNAL USE ONLY** - creates a LinkId without validation
    ///
    /// # ⚠️ WARNING ⚠️
    ///
    /// This function bypasses all validation. It exists **only** for:
    /// - Macro-generated code that constructs IDs in known-valid formats
    /// - Runtime internals where validation would be redundant
    ///
    /// # Safety Contract
    ///
    /// Caller **must guarantee** the string is valid:
    /// - Non-empty
    /// - Only contains: alphanumeric, `_`, `-`, `.`, `>`, `:`
    ///
    /// Violating this contract will cause **debug assertions to panic** and may
    /// cause undefined behavior in release builds.
    ///
    /// # For External Users
    ///
    /// **Do not use this function.** Use [`LinkId::from_string`] instead.
    ///
    /// # Debug Assertions
    ///
    /// In debug builds, validates input to catch misuse.
    /// In release builds, validation is skipped for performance.
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

/// A validated, unique link identifier
///
/// Cannot be constructed directly from arbitrary strings - must go through validation
/// via [`LinkId::from_string`]. This ensures all LinkIds in the system
/// are valid and prevents mixing up link IDs with other string types.
///
/// # Examples
///
/// ```rust,ignore
/// use streamlib::core::LinkId;
///
/// // Parse and validate from string (type guard pattern)
/// let link_id = LinkId::from_string("source.video_out->dest.video_in")?;
///
/// // Use in method calls
/// output_port.add_link(link_id, producer, wakeup)?;
///
/// // Works with &str comparisons due to Deref
/// if link_id.as_str() == "my_link" {
///     // ...
/// }
/// ```
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct LinkId(String);

/// Errors that can occur when parsing a LinkId
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkIdError {
    /// Link ID is empty
    Empty,
    /// Link ID contains invalid characters
    InvalidCharacters(String),
    /// Link ID has invalid format
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
    /// Parse and validate a link ID from a string
    ///
    /// This is the **only public way** to create a LinkId from arbitrary input.
    /// Acts as a "type guard" - if this returns Ok, you have a valid LinkId.
    ///
    /// # Validation Rules
    ///
    /// - Must not be empty
    /// - Must contain only alphanumeric characters, underscore, hyphen, or dot
    ///
    /// # Examples
    ///
    /// ```
    /// use streamlib::core::LinkId;
    ///
    /// // Valid IDs
    /// assert!(LinkId::from_string("simple_id").is_ok());
    /// assert!(LinkId::from_string("proc1.out->proc2.in").is_ok());
    /// assert!(LinkId::from_string("link-123").is_ok());
    ///
    /// // Invalid IDs
    /// assert!(LinkId::from_string("").is_err());
    /// assert!(LinkId::from_string("invalid spaces").is_err());
    /// assert!(LinkId::from_string("bad@char").is_err());
    /// ```
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
