// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{LinkInput, LinkOutput, Result, RuntimeContext};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9000,
        }
    }
}

#[crate::processor(
    execution = Manual,
    description = "Runtime api server for streamlib"
)]
pub struct ApiServerProcessor {
    #[crate::config]
    config: ApiServerConfig,
}

impl ApiServerProcessor::Processor {
    fn process(&mut self) -> Result<()> {
        // start our tokio loop
    }
}
