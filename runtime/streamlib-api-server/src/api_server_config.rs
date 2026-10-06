// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The api-server processor's configuration type — the seam its encoding is
//! pinned at.

use serde::{Deserialize, Serialize};
use streamlib::sdk::schemars::JsonSchema;

/// Configuration for the runtime API server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct ApiServerConfig {
    /// Log file path for surface-share registration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
}

#[cfg(test)]
mod api_server_config_encoding_tests {
    use super::ApiServerConfig;

    /// Golden document: every field present, in declaration order.
    const FULLY_POPULATED: &str = r#"{"log_path":"/tmp/node.jsonl"}"#;

    /// A fully-populated config survives a decode/encode round trip unchanged.
    #[test]
    fn a_fully_populated_config_round_trips() {
        let decoded: ApiServerConfig = serde_json::from_str(FULLY_POPULATED).unwrap();
        assert_eq!(decoded.log_path.as_deref(), Some("/tmp/node.jsonl"));
        assert_eq!(serde_json::to_string(&decoded).unwrap(), FULLY_POPULATED);
    }

    /// An absent optional is omitted from the encoding, never written as null,
    /// and an empty document decodes to it.
    #[test]
    fn absent_optionals_are_omitted_not_nulled() {
        let config = ApiServerConfig::default();
        assert_eq!(serde_json::to_string(&config).unwrap(), "{}");
        assert_eq!(
            serde_json::from_str::<ApiServerConfig>("{}").unwrap(),
            config
        );
    }
}
