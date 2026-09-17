// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::ffi::OsStr;
use std::fmt;
use std::ops::Deref;

use crate::core::error::{Error, Result};

/// The environment variable that pins a runtime's id.
pub const RUNTIME_ID_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_RUNTIME_ID";

/// The most bytes a pinned runtime id may take.
///
/// The id names the runtime's log file, its surface-sharing socket and its
/// iceoryx2 node, and the socket path is the tightest of the three.
pub const PINNED_RUNTIME_ID_MAX_BYTES: usize = 64;

/// Unique identifier for a runtime instance.
///
/// Generated automatically or loaded from `STREAMLIB_RUNTIME_ID` environment variable.
/// Use stable IDs in production for consistent cache paths across restarts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeUniqueId(String);

impl RuntimeUniqueId {
    /// Load from `STREAMLIB_RUNTIME_ID`, refusing a value that is not a runtime id, or generate one.
    ///
    /// An empty value reads as unset, as an empty `XDG_RUNTIME_DIR` does.
    pub fn from_env_or_generate() -> Result<Self> {
        match std::env::var_os(RUNTIME_ID_ENVIRONMENT_VARIABLE) {
            Some(pinned) if !pinned.is_empty() => {
                let runtime_id = Self::from_pinned(&pinned)?;
                tracing::info!(
                    "Using runtime ID from {RUNTIME_ID_ENVIRONMENT_VARIABLE}: {}",
                    runtime_id
                );
                Ok(runtime_id)
            }
            _ => {
                let runtime_id = Self::generate();
                tracing::trace!(
                    "Generated runtime ID: {}. Set {RUNTIME_ID_ENVIRONMENT_VARIABLE} for stable IDs in production.",
                    runtime_id
                );
                Ok(runtime_id)
            }
        }
    }

    fn generate() -> Self {
        Self(format!("R{}", cuid2::create_id()))
    }

    /// A pinned id, refused by name unless it is 1 to [`PINNED_RUNTIME_ID_MAX_BYTES`]
    /// bytes of ASCII letters, digits, `.`, `_` and `-` not starting with `.`.
    pub(crate) fn from_pinned(pinned: &OsStr) -> Result<Self> {
        let refusal = |what_is_wrong: String| {
            Error::Configuration(format!(
                "{RUNTIME_ID_ENVIRONMENT_VARIABLE}={pinned:?} is not a runtime id: {what_is_wrong}. \
                 A runtime id is 1 to {PINNED_RUNTIME_ID_MAX_BYTES} bytes of ASCII letters, digits, \
                 '.', '_' and '-', and does not start with '.'"
            ))
        };
        let Some(pinned) = pinned.to_str() else {
            return Err(refusal("it is not UTF-8".to_string()));
        };
        if pinned.is_empty() {
            return Err(refusal("it is empty".to_string()));
        }
        if pinned.len() > PINNED_RUNTIME_ID_MAX_BYTES {
            return Err(refusal(format!("it takes {} bytes", pinned.len())));
        }
        if let Some(refused_character) = pinned.chars().find(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
        }) {
            return Err(refusal(format!("it contains {refused_character:?}")));
        }
        if pinned.starts_with('.') {
            return Err(refusal("it starts with '.'".to_string()));
        }
        Ok(Self(pinned.to_string()))
    }

    /// Get the inner string value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for RuntimeUniqueId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<str> for RuntimeUniqueId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for RuntimeUniqueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuntimeUniqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RuntimeUniqueId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RuntimeUniqueId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<RuntimeUniqueId> for String {
    fn from(id: RuntimeUniqueId) -> Self {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn refusal_of(pinned: &OsStr) -> String {
        RuntimeUniqueId::from_pinned(pinned)
            .expect_err("the pinned id must be refused")
            .to_string()
    }

    #[test]
    fn a_pinned_id_of_letters_digits_dots_underscores_and_dashes_is_taken_as_written() {
        for pinned in ["camera-2", "R1a2b3", "rig_node.7", "x"] {
            assert_eq!(
                RuntimeUniqueId::from_pinned(OsStr::new(pinned))
                    .expect("a legal pinned id")
                    .as_str(),
                pinned
            );
        }
    }

    #[test]
    fn a_pinned_id_that_could_leave_a_directory_is_refused_naming_the_variable_and_the_character() {
        for (pinned, named) in [("../escape", "'/'"), ("a/b", "'/'"), ("two words", "' '")] {
            let refusal = refusal_of(OsStr::new(pinned));
            assert!(
                refusal.contains(RUNTIME_ID_ENVIRONMENT_VARIABLE) && refusal.contains(named),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_pinned_id_starting_with_a_dot_is_refused() {
        assert!(refusal_of(OsStr::new(".hidden")).contains("starts with '.'"));
    }

    #[test]
    fn a_pinned_id_past_the_byte_limit_is_refused_and_one_at_it_is_taken() {
        let at_the_limit = "a".repeat(PINNED_RUNTIME_ID_MAX_BYTES);
        assert!(RuntimeUniqueId::from_pinned(OsStr::new(&at_the_limit)).is_ok());

        let past_the_limit = "a".repeat(PINNED_RUNTIME_ID_MAX_BYTES + 1);
        assert!(
            refusal_of(OsStr::new(&past_the_limit))
                .contains(&format!("{} bytes", PINNED_RUNTIME_ID_MAX_BYTES + 1))
        );
    }

    #[test]
    fn an_empty_or_non_utf8_pinned_id_is_refused() {
        assert!(refusal_of(OsStr::new("")).contains("empty"));
        assert!(refusal_of(OsStr::from_bytes(b"id-\xff")).contains("not UTF-8"));
    }

    #[test]
    fn a_generated_id_is_one_a_pin_would_accept() {
        let generated = RuntimeUniqueId::generate();
        assert_eq!(
            RuntimeUniqueId::from_pinned(OsStr::new(generated.as_str()))
                .expect("a generated id is a legal pinned id"),
            generated
        );
    }
}
