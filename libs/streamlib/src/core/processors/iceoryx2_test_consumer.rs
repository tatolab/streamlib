// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test consumer processor for iceoryx2-based communication validation.

use serde::{Deserialize, Serialize};

use crate::core::Result;

/// Configuration for the iceoryx2 test consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
#[serde(default)]
pub struct Iceoryx2TestConsumerConfig {
    /// Whether to log received messages.
    pub log_messages: bool,
}

impl Default for Iceoryx2TestConsumerConfig {
    fn default() -> Self {
        Self {
            log_messages: false,
        }
    }
}

/// Test consumer that receives raw bytes via iceoryx2.
///
/// Uses the new `inputs = [...]` syntax for iceoryx2-based communication.
#[crate::processor(
    execution = Reactive,
    description = "Test consumer for iceoryx2 communication validation",
    inputs = [input("data_in", schema = "com.streamlib.test.rawdata@1.0.0")]
)]
pub struct Iceoryx2TestConsumerProcessor {
    #[crate::config]
    config: Iceoryx2TestConsumerConfig,

    /// Count of received messages.
    received_count: u64,
}

impl crate::core::ReactiveProcessor for Iceoryx2TestConsumerProcessor::Processor {
    fn process(&mut self) -> Result<()> {
        // Read from input mailbox
        while let Some(payload) = self.inputs.read("data_in") {
            self.received_count += 1;

            if self.config.log_messages {
                let data = payload.data();
                if data.len() >= 8 {
                    let counter = u64::from_le_bytes(data[..8].try_into().unwrap());
                    tracing::info!(
                        "Received message #{}: counter={}",
                        self.received_count,
                        counter
                    );
                }
            }
        }
        Ok(())
    }
}

impl Iceoryx2TestConsumerProcessor::Processor {
    /// Get the total count of received messages.
    pub fn received_count(&self) -> u64 {
        self.received_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = Iceoryx2TestConsumerConfig::default();
        assert!(!config.log_messages);
    }
}
