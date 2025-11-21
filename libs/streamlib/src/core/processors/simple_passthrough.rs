use crate::core::{Result, StreamInput, StreamOutput, VideoFrame};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib_macros::StreamProcessor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimplePassthroughConfig {
    pub scale: f32,
}

impl Default for SimplePassthroughConfig {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

// NEW PATTERN: Complete trait generation - always generates implementations!
#[derive(StreamProcessor)]
#[processor(
    mode = Pull,
    description = "Passes video frames through unchanged (for testing)"
)]
pub struct SimplePassthroughProcessor {
    #[input(description = "Input video stream")]
    input: StreamInput<VideoFrame>,

    #[output(description = "Output video stream")]
    output: Arc<StreamOutput<VideoFrame>>,

    #[config]
    config: SimplePassthroughConfig,
}

// Only business logic implementation needed!
impl SimplePassthroughProcessor {
    // Lifecycle - auto-detected by macro (empty implementations for simple processor)
    fn setup(&mut self, _ctx: &crate::core::RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    // Business logic - called by macro-generated process()
    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input.read_latest() {
            self.output.write(frame);
        }
        Ok(())
    }
}

impl SimplePassthroughProcessor {
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

    // Note: SimplePassthroughProcessor uses the StreamProcessor macro which generates
    // from_config(), descriptor(), and other trait implementations.
    // These are tested indirectly through integration tests and actual usage in the runtime.
    // Direct unit testing of macro-generated code is not practical here as the macro
    // generates code at compile time and the generated methods may have specific signatures
    // that don't match simple test expectations.
}
