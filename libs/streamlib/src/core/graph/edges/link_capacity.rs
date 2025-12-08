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
}

impl From<usize> for LinkCapacity {
    fn from(value: usize) -> Self {
        Self(value)
    }
}
