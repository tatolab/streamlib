// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkCapacity(usize);

impl Default for LinkCapacity {
    fn default() -> Self {
        LinkCapacity(4)
    }
}

impl LinkCapacity {
    pub fn get(&self) -> usize {
        self.0
    }
    pub fn index(&self) -> usize {
        self.0
    }
}

impl From<usize> for LinkCapacity {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

impl From<LinkCapacity> for usize {
    fn from(cap: LinkCapacity) -> Self {
        cap.0
    }
}

impl std::ops::Deref for LinkCapacity {
    type Target = usize;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
