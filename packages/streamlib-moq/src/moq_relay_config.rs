// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a session connects, and the checks that fail here rather than on the
//! wire.

use crate::error::{MoqExtensionError, Result};

/// The MoQ Transport draft this wheel speaks, offered as the WebTransport
/// subprotocol on the extended CONNECT. Draft-16 moved version negotiation out
/// of the SETUP message, so this string is the whole of it: a relay that does
/// not accept it refuses the connection rather than mis-negotiating.
pub(crate) fn moq_transport_subprotocol() -> Result<&'static str> {
    std::str::from_utf8(moq_transport::setup::ALPN).map_err(|_| MoqExtensionError::Refused {
        what: "moq-transport's subprotocol name is not UTF-8, so no draft can be offered on the \
               extended CONNECT"
            .to_owned(),
    })
}

/// The longest `:path` the WebTransport CONNECT accepts. A relay token rides
/// the path, so a long signed token fails here — with the length named —
/// instead of inside the transport as an opaque invalid-path error.
const LONGEST_CONNECT_PATH_BYTES: usize = 1024;

/// Where a publishing or subscribing session connects, and under what
/// broadcast name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoqRelayConfig {
    /// The relay endpoint, token included: draft-16 carries the credential as
    /// the URL's path and nowhere else.
    pub(crate) relay_endpoint_url: String,
    /// The broadcast namespace. Case-sensitive, no leading or trailing slash —
    /// a leading one builds an empty first namespace field, which fails at send
    /// time rather than at construction.
    pub(crate) broadcast_path: String,
}

impl MoqRelayConfig {
    /// The URL to dial, with the broadcast namespace deliberately *not* in it.
    ///
    /// The path is where the relay's auth token lives, so the namespace cannot
    /// share it — it travels in `PUBLISH_NAMESPACE` / `SUBSCRIBE` instead.
    pub(crate) fn dial_url(&self) -> Result<url::Url> {
        // The value is never echoed: `relay_url` carries the relay's auth
        // token as its path, and a refusal reaches the parent's log through
        // the helper's log sink. `failure` already names what is malformed.
        let parsed = url::Url::parse(&self.relay_endpoint_url).map_err(|failure| {
            MoqExtensionError::Refused {
                what: format!(
                    "`relay_url` is not a URL ({failure}); it reads \
                     `https://<relay host>/<token>`"
                ),
            }
        })?;
        if parsed.scheme() != "https" {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "`relay_url` must be https for a WebTransport relay; got `{}`",
                    parsed.scheme()
                ),
            });
        }
        let path = parsed.path();
        if path.len() > LONGEST_CONNECT_PATH_BYTES {
            return Err(MoqExtensionError::Refused {
                what: format!(
                    "`relay_url`'s path is {} bytes and the WebTransport CONNECT accepts at most \
                     {LONGEST_CONNECT_PATH_BYTES}; a relay token this long cannot be carried",
                    path.len()
                ),
            });
        }
        if path.contains('%') {
            return Err(MoqExtensionError::Refused {
                what: "`relay_url`'s path contains a percent-encoded character, which the \
                       WebTransport CONNECT refuses; a relay token must be percent-free"
                    .to_owned(),
            });
        }
        Ok(parsed)
    }

    /// The broadcast as a MoQ namespace.
    ///
    /// Built through `try_from` rather than `from_utf8_path`, which keeps empty
    /// fields: a broadcast written with a leading slash would then be accepted
    /// here and refused at send time with `EmptyNamespaceField`, a failure with
    /// nothing pointing back at the config that caused it.
    pub(crate) fn namespace(&self) -> Result<moq_transport::coding::TrackNamespace> {
        let trimmed = self.broadcast_path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(MoqExtensionError::Refused {
                what: "`broadcast` is empty; a MoQ namespace needs at least one field".to_owned(),
            });
        }
        moq_transport::coding::TrackNamespace::try_from(trimmed).map_err(|failure| {
            MoqExtensionError::Refused {
                what: format!("`broadcast` is not a MoQ namespace: {trimmed} ({failure})"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(relay: &str, broadcast: &str) -> MoqRelayConfig {
        MoqRelayConfig {
            relay_endpoint_url: relay.to_owned(),
            broadcast_path: broadcast.to_owned(),
        }
    }

    #[test]
    fn the_dial_url_keeps_the_relay_token_that_rides_the_path() {
        let url = config("https://relay.example/tok3n", "streamlib/abc")
            .dial_url()
            .expect("an https URL with a plain path dials");
        assert_eq!(url.path(), "/tok3n");
    }

    #[test]
    fn the_broadcast_never_reaches_the_dial_url() {
        // 0.14 appended it. Draft-16 cannot: that space belongs to the token.
        let url = config("https://relay.example/tok3n", "streamlib/abc")
            .dial_url()
            .unwrap();
        assert!(!url.path().contains("streamlib"));
    }

    #[test]
    fn no_refusal_from_the_dial_url_ever_echoes_the_relay_token() {
        // `relay_url` carries the credential, and every refusal here reaches
        // the parent process's log.
        const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.a-live-looking-token.sig";
        for relay in [
            // no scheme at all — the ordinary first-run typo
            format!("draft-16.example/{TOKEN}"),
            format!("moqt://relay.example/{TOKEN}"),
            format!("https://relay.example/{}", "a".repeat(2000)),
            format!("https://relay.example/to%2F{TOKEN}"),
        ] {
            let refusal = MoqRelayConfig {
                relay_endpoint_url: relay.clone(),
                broadcast_path: "streamlib/a".to_owned(),
            }
            .dial_url()
            .expect_err("each of these is refused");
            assert!(
                !refusal.to_string().contains(TOKEN),
                "a refusal put the relay token in the log: {refusal}"
            );
        }
    }

    #[test]
    fn a_percent_encoded_token_is_refused_by_name_before_dialling() {
        let refusal = config("https://relay.example/to%2Fken", "b")
            .dial_url()
            .expect_err("a percent-encoded path cannot ride a CONNECT");
        assert!(refusal.to_string().contains("percent"), "{refusal}");
    }

    #[test]
    fn a_token_past_the_connect_path_ceiling_is_refused_by_name() {
        let long = "a".repeat(LONGEST_CONNECT_PATH_BYTES + 1);
        let refusal = config(&format!("https://relay.example/{long}"), "b")
            .dial_url()
            .expect_err("an over-long path cannot ride a CONNECT");
        assert!(refusal.to_string().contains("1024"), "{refusal}");
    }

    #[test]
    fn a_non_https_relay_is_refused_rather_than_dialled() {
        let refusal = config("moqt://relay.example/tok3n", "b")
            .dial_url()
            .expect_err("raw QUIC is not the transport this wheel speaks");
        assert!(refusal.to_string().contains("https"), "{refusal}");
    }

    #[test]
    fn a_broadcast_written_with_a_leading_slash_still_makes_a_namespace() {
        // `from_utf8_path` would keep the empty first field and fail at send
        // time, far from the config that caused it.
        let namespace = config("https://relay.example/t", "/streamlib/abc")
            .namespace()
            .expect("the leading slash is trimmed, not carried");
        assert_eq!(
            namespace,
            config("https://r.example/t", "streamlib/abc")
                .namespace()
                .unwrap()
        );
    }

    #[test]
    fn an_empty_broadcast_is_refused_by_name() {
        let refusal = config("https://relay.example/t", "///")
            .namespace()
            .expect_err("a namespace needs at least one field");
        assert!(refusal.to_string().contains("broadcast"), "{refusal}");
    }

    #[test]
    fn the_subprotocol_is_the_draft_this_wheel_speaks() {
        assert_eq!(moq_transport_subprotocol().unwrap(), "moqt-16");
    }
}
