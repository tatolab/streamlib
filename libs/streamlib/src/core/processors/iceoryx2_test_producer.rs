// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test producer processor for iceoryx2-based communication validation.

use serde::{Deserialize, Serialize};

use crate::core::Result;

/// Configuration for the iceoryx2 test producer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
#[serde(default)]
pub struct Iceoryx2TestProducerConfig {
    /// Number of messages to produce per process() call.
    pub messages_per_call: usize,
}

impl Default for Iceoryx2TestProducerConfig {
    fn default() -> Self {
        Self {
            messages_per_call: 1,
        }
    }
}

/// Test producer that sends raw bytes via iceoryx2.
///
/// Uses the new `outputs = [...]` syntax for iceoryx2-based communication.
#[crate::processor(
    execution = Continuous,
    description = "Test producer for iceoryx2 communication validation",
    outputs = [output("data_out", schema = "com.streamlib.test.rawdata@1.0.0")]
)]
pub struct Iceoryx2TestProducerProcessor {
    #[crate::config]
    config: Iceoryx2TestProducerConfig,

    /// Counter for generating test data.
    counter: u64,
}

impl crate::core::ContinuousProcessor for Iceoryx2TestProducerProcessor::Processor {
    fn process(&mut self) -> Result<()> {
        for _ in 0..self.config.messages_per_call {
            self.counter += 1;

            // Create test payload: 8-byte counter + timestamp
            let data = self.counter.to_le_bytes();

            // Write to output via iceoryx2
            self.outputs.write("data_out", &data)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = Iceoryx2TestProducerConfig::default();
        assert_eq!(config.messages_per_call, 1);
    }
}
