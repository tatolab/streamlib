// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test consumer processor for iceoryx2-based communication validation.

use serde::{Deserialize, Serialize};

use crate::core::Result;

/// Configuration for the iceoryx2 test consumer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
#[serde(default)]
pub struct Iceoryx2TestConsumerConfig {
    /// Whether to log received messages.
    pub log_messages: bool,
}

/// Test consumer that receives raw bytes via iceoryx2.
#[crate::processor("schemas/processors/iceoryx2_test_consumer.yaml")]
pub struct Iceoryx2TestConsumerProcessor;

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
