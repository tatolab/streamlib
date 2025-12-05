// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{LinkInput, LinkOutput, Result, RuntimeContext, VideoFrame};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimplePassthroughConfig {
    pub scale: f32,
}

impl Default for SimplePassthroughConfig {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

#[crate::processor(
    execution = Manual,
    description = "Passes video frames through unchanged (for testing)"
)]
pub struct SimplePassthroughProcessor {
    #[crate::input(description = "Input video stream")]
    input: LinkInput<VideoFrame>,

    #[crate::output(description = "Output video stream")]
    output: Arc<LinkOutput<VideoFrame>>,

    #[crate::config]
    config: SimplePassthroughConfig,
}

impl SimplePassthroughProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input.read() {
            self.output.write(frame);
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
