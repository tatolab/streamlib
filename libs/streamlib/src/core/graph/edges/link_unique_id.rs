// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkUniqueId(String);

impl LinkUniqueId {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for LinkUniqueId {
    fn default() -> Self {
        Self(format!("L{}", cuid2::create_id()))
    }
}

impl Deref for LinkUniqueId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<str> for LinkUniqueId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for LinkUniqueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LinkUniqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for LinkUniqueId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&String> for LinkUniqueId {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

impl From<&str> for LinkUniqueId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<LinkUniqueId> for String {
    fn from(id: LinkUniqueId) -> Self {
        id.0
    }
}

impl From<&LinkUniqueId> for String {
    fn from(id: &LinkUniqueId) -> Self {
        id.0.clone()
    }
}

impl From<&LinkUniqueId> for LinkUniqueId {
    fn from(id: &LinkUniqueId) -> Self {
        Self(id.0.clone())
    }
}

impl PartialEq<str> for LinkUniqueId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<LinkUniqueId> for str {
    fn eq(&self, other: &LinkUniqueId) -> bool {
        self == other.0
    }
}

impl PartialEq<&str> for LinkUniqueId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<LinkUniqueId> for &str {
    fn eq(&self, other: &LinkUniqueId) -> bool {
        *self == other.0
    }
}

impl PartialEq<String> for LinkUniqueId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<LinkUniqueId> for String {
    fn eq(&self, other: &LinkUniqueId) -> bool {
        *self == other.0
    }
}
