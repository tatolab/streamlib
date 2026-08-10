// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The api-server processor's configuration type — the seam its encoding is
//! pinned at.

pub use crate::_generated_::ApiServerConfig;

#[cfg(test)]
mod api_server_config_encoding_tests {
    use super::ApiServerConfig;

    /// Golden document: every field present, in declaration order.
    const FULLY_POPULATED: &str = r#"{"host":"0.0.0.0","port":8080,"log_path":"/tmp/node.jsonl","name":"node-a","require_auth":true}"#;

    /// A fully-populated config survives a decode/encode round trip unchanged.
    #[test]
    fn a_fully_populated_config_round_trips() {
        let decoded: ApiServerConfig = serde_json::from_str(FULLY_POPULATED).unwrap();
        assert_eq!(decoded.host, "0.0.0.0");
        assert_eq!(decoded.port, 8080);
        assert_eq!(decoded.log_path.as_deref(), Some("/tmp/node.jsonl"));
        assert_eq!(decoded.name.as_deref(), Some("node-a"));
        assert_eq!(decoded.require_auth, Some(true));
        assert_eq!(serde_json::to_string(&decoded).unwrap(), FULLY_POPULATED);
    }

    /// An absent optional is omitted from the encoding, never written as null.
    #[test]
    fn absent_optionals_are_omitted_not_nulled() {
        let config = ApiServerConfig {
            host: "127.0.0.1".to_string(),
            port: 3000,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&config).unwrap(),
            r#"{"host":"127.0.0.1","port":3000}"#
        );
    }
}
