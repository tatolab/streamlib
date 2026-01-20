// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
#[serde(default)]
pub struct SimplePassthroughConfig {
    pub scale: f32,
}

impl Default for SimplePassthroughConfig {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

#[crate::processor("schemas/processors/simple_passthrough.yaml")]
pub struct SimplePassthroughProcessor;

impl crate::core::ManualProcessor for SimplePassthroughProcessor::Processor {
    // Uses default setup() and teardown() implementations from Processor trait

    fn start(&mut self) -> Result<()> {
        // Read from iceoryx2 input mailbox and write to output
        if let Some(payload) = self.inputs.get("input") {
            self.outputs.write("output", payload.data())?;
        }
        Ok(())
    }
}

impl SimplePassthroughProcessor::Processor {
    pub fn scale(&self) -> f32 {
        self.config.scale
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.config.scale = scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = SimplePassthroughConfig::default();
        assert_eq!(config.scale, 1.0);
    }

    #[test]
    fn test_config_custom() {
        let config = SimplePassthroughConfig { scale: 2.5 };
        assert_eq!(config.scale, 2.5);
    }
}
