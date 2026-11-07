// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A gateway's relay, read from its config and the settings its `settings`
//! input replaces them with: what each relay kind is dialed with, where its
//! credential rides, when it is renewed and how often a catalog is written
//! again whole there.

use std::time::{Duration, Instant, SystemTime};

use moq_auth::{Claims, Key, KeyOperation, Pattern};
use serde_json::{Map, Value};
use url::Url;

use crate::error::{MoqModuleError, Result};

/// The query parameter a moq-auth server reads a session's token from.
const RELAY_DIAL_TOKEN_QUERY_PARAMETER: &str = "jwt";

/// The suffix of a broadcast whose catalog the stack's players read.
pub const BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ: &str = ".hang";

/// moq.pro's relay, under which each project is one path segment.
pub const MOQPRO_RELAY_URL: &str = "https://cdn.moq.pro";

/// How long a token minted for moq.pro is valid.
pub const MOQPRO_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// How long after minting a moq.pro token is replaced by a fresh one.
pub const MOQPRO_TOKEN_RENEWED_AFTER: Duration = Duration::from_secs(20 * 60 * 60);

/// How often a broadcast's catalog is written again whole at Cloudflare, whose
/// relay hands a joining subscriber only the newest group.
pub const CLOUDFLARE_CATALOG_IS_REWRITTEN_WHOLE_EVERY: Duration = Duration::from_secs(1);

const KIND_KEY: &str = "kind";
const BROADCAST_KEY: &str = "broadcast";
const RELAY_URL_KEY: &str = "relay_url";
const TOKEN_KEY: &str = "token";
const CERTIFICATE_FINGERPRINT_KEY: &str = "certificate_fingerprint";
const PROJECT_ID_KEY: &str = "project_id";
const SIGNING_KEY_KEY: &str = "signing_key";

const KEYS_EVERY_KIND_TAKES: [&str; 2] = [KIND_KEY, BROADCAST_KEY];

/// The relay a gateway dials, by kind, with the kind's own settings.
#[derive(Clone, PartialEq, Eq)]
pub enum MoqGatewayRelay {
    /// A `moq-relay` a person or a rig runs, dialed at `relay_url`.
    MoqRelay {
        relay_url: Url,
        token: Option<String>,
        certificate_fingerprint: Option<String>,
    },
    /// A relay Cloudflare provisioned, dialed at its publish-and-subscribe URL,
    /// whose last path segment is its token.
    Cloudflare { relay_url: Url },
    /// A moq.pro project, dialed with a token signed by the project's key.
    MoqPro {
        project_id: String,
        signing_key_jwk: String,
    },
}

impl std::fmt::Debug for MoqGatewayRelay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.kind())
    }
}

/// A gateway's relay and the broadcast it publishes there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqGatewayRelaySettings {
    pub relay: MoqGatewayRelay,
    /// The broadcast's name, one path chunk, before the catalog suffix.
    pub broadcast: String,
}

/// How a gateway dials its relay once.
#[derive(Clone)]
pub struct MoqGatewayRelayDial {
    /// The relay's URL with its credential in it; never logged.
    pub dial_url_carrying_the_credential: Url,
    /// The relay's origin alone, the one part of its URL a log names.
    pub relay_origin: String,
    /// SHA-256 fingerprints of a self-signed certificate the relay is pinned to.
    pub certificate_fingerprints: Vec<String>,
    /// When the credential must be replaced by a fresh one, if ever.
    pub renew_by: Option<Instant>,
    /// How often the broadcast's catalog is written again whole, if ever.
    pub catalog_rewritten_whole_every: Option<Duration>,
    /// The broadcast's path at the relay, under the credential's root.
    pub broadcast_path: String,
}

impl std::fmt::Debug for MoqGatewayRelayDial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MoqGatewayRelayDial")
            .field("relay_origin", &self.relay_origin)
            .field("broadcast_path", &self.broadcast_path)
            .finish_non_exhaustive()
    }
}

impl MoqGatewayRelay {
    /// The kind's name as a gateway's `kind` config key spells it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MoqRelay { .. } => "moq-relay",
            Self::Cloudflare { .. } => "cloudflare",
            Self::MoqPro { .. } => "moqpro",
        }
    }
}

impl MoqGatewayRelaySettings {
    /// Read a gateway's settings from `settings`, its config with any values
    /// its `settings` input replaced; a key left out or `null` is unset.
    pub fn read_from(settings: &Map<String, Value>) -> Result<Self> {
        let kind = required_string(settings, KIND_KEY)?;
        let keys_the_kind_takes: &[&str] = match kind.as_str() {
            "moq-relay" => &[RELAY_URL_KEY, TOKEN_KEY, CERTIFICATE_FINGERPRINT_KEY],
            "cloudflare" => &[RELAY_URL_KEY],
            "moqpro" => &[PROJECT_ID_KEY, SIGNING_KEY_KEY],
            other => {
                return Err(refused(format!(
                    "`kind` is `{other}`; a gateway dials `moq-relay`, `cloudflare` or `moqpro`"
                )));
            }
        };
        for (key, value) in settings {
            if value.is_null()
                || KEYS_EVERY_KIND_TAKES.contains(&key.as_str())
                || keys_the_kind_takes.contains(&key.as_str())
            {
                continue;
            }
            return Err(refused(format!(
                "`{key}` is set, and a `{kind}` gateway takes {}",
                keys_the_kind_takes
                    .iter()
                    .chain(&KEYS_EVERY_KIND_TAKES)
                    .map(|key| format!("`{key}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let broadcast = required_string(settings, BROADCAST_KEY)?;
        if broadcast.is_empty() || broadcast.contains('/') {
            return Err(refused(format!(
                "`broadcast` is `{broadcast}`; it is one non-empty name with no `/`"
            )));
        }
        let relay = match kind.as_str() {
            "moq-relay" => MoqGatewayRelay::MoqRelay {
                relay_url: relay_url_of(settings)?,
                token: optional_string(settings, TOKEN_KEY)?,
                certificate_fingerprint: optional_string(settings, CERTIFICATE_FINGERPRINT_KEY)?,
            },
            "cloudflare" => {
                let relay_url = relay_url_of(settings)?;
                refuse_a_cloudflare_url_carrying_no_token(&relay_url)?;
                MoqGatewayRelay::Cloudflare { relay_url }
            }
            _ => {
                let project_id = required_string(settings, PROJECT_ID_KEY)?.trim().to_string();
                if project_id.is_empty() || project_id.contains('/') {
                    return Err(refused(format!(
                        "`project_id` `{project_id}` is not a moq.pro project id: one path \
                         segment, non-empty, with no `/`"
                    )));
                }
                let signing_key_jwk = required_string(settings, SIGNING_KEY_KEY)?;
                MoqProProjectSigningKey::read_from(&signing_key_jwk)?;
                MoqGatewayRelay::MoqPro {
                    project_id,
                    signing_key_jwk,
                }
            }
        };
        Ok(Self { relay, broadcast })
    }

    /// The broadcast's path at the relay.
    pub fn broadcast_path(&self) -> String {
        format!(
            "{}{BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ}",
            self.broadcast
        )
    }

    /// How to dial the relay now, minting a fresh credential where the kind
    /// mints one.
    pub fn dial_now(&self) -> Result<MoqGatewayRelayDial> {
        let broadcast_path = self.broadcast_path();
        let (dial_url_carrying_the_credential, certificate_fingerprints, renew_by, rewrite) =
            match &self.relay {
                MoqGatewayRelay::MoqRelay {
                    relay_url,
                    token,
                    certificate_fingerprint,
                } => {
                    let mut dial_url = relay_url.clone();
                    if let Some(token) = token {
                        dial_url
                            .query_pairs_mut()
                            .append_pair(RELAY_DIAL_TOKEN_QUERY_PARAMETER, token);
                    }
                    (
                        dial_url,
                        certificate_fingerprint.iter().cloned().collect(),
                        None,
                        None,
                    )
                }
                MoqGatewayRelay::Cloudflare { relay_url } => (
                    relay_url.clone(),
                    Vec::new(),
                    None,
                    Some(CLOUDFLARE_CATALOG_IS_REWRITTEN_WHOLE_EVERY),
                ),
                MoqGatewayRelay::MoqPro {
                    project_id,
                    signing_key_jwk,
                } => {
                    let token = MoqProProjectSigningKey::read_from(signing_key_jwk)?
                        .mint_a_token_for_the_broadcast(project_id, &broadcast_path)?;
                    let mut dial_url = moqpro_project_dial_url(project_id)?;
                    dial_url
                        .query_pairs_mut()
                        .append_pair(RELAY_DIAL_TOKEN_QUERY_PARAMETER, &token);
                    (
                        dial_url,
                        Vec::new(),
                        Some(Instant::now() + MOQPRO_TOKEN_RENEWED_AFTER),
                        None,
                    )
                }
            };
        Ok(MoqGatewayRelayDial {
            relay_origin: dial_url_carrying_the_credential
                .origin()
                .ascii_serialization(),
            dial_url_carrying_the_credential,
            certificate_fingerprints,
            renew_by,
            catalog_rewritten_whole_every: rewrite,
            broadcast_path,
        })
    }
}

fn refused(what: String) -> MoqModuleError {
    MoqModuleError::Refused {
        what: format!("MoqGateway: {what}"),
    }
}

fn optional_string(settings: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match settings.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(refused(format!("`{key}` holds {other}, and it takes a string"))),
    }
}

fn required_string(settings: &Map<String, Value>, key: &str) -> Result<String> {
    optional_string(settings, key)?.ok_or_else(|| refused(format!("`{key}` is not set")))
}

/// The relay's URL, refused by name without echoing it, since a credential
/// may ride it.
fn relay_url_of(settings: &Map<String, Value>) -> Result<Url> {
    let relay_url = required_string(settings, RELAY_URL_KEY)?;
    Url::parse(relay_url.trim()).map_err(|unparsed| {
        refused(format!("`relay_url` is not a URL ({unparsed})"))
    })
}

fn refuse_a_cloudflare_url_carrying_no_token(relay_url: &Url) -> Result<()> {
    if relay_url.scheme() != "https" {
        return Err(refused("`relay_url` is not an `https://` URL".to_string()));
    }
    if relay_url.query().is_some() || relay_url.fragment().is_some() {
        return Err(refused(
            "`relay_url` carries a query or a fragment, which a Cloudflare relay URL never does"
                .to_string(),
        ));
    }
    let carries_a_token = relay_url
        .path_segments()
        .is_some_and(|mut segments| segments.any(|segment| !segment.is_empty()));
    if !carries_a_token {
        return Err(refused(
            "`relay_url` carries no token in its path; it is the relay's publish-and-subscribe \
             URL Cloudflare returned, its token the last path segment"
                .to_string(),
        ));
    }
    Ok(())
}

fn moqpro_project_dial_url(project_id: &str) -> Result<Url> {
    let mut dial_url = Url::parse(MOQPRO_RELAY_URL).map_err(|unparsed| MoqModuleError::Relay {
        what: format!("moq.pro's relay URL does not parse: {unparsed}"),
    })?;
    dial_url
        .path_segments_mut()
        .map_err(|()| MoqModuleError::Relay {
            what: "moq.pro's relay URL has no path".to_string(),
        })?
        .pop_if_empty()
        .push(project_id);
    Ok(dial_url)
}

/// A moq.pro project's key, read from the JWK moq.pro gave.
struct MoqProProjectSigningKey(Key);

impl MoqProProjectSigningKey {
    fn read_from(signing_key_jwk: &str) -> Result<Self> {
        let key = Key::from_str(signing_key_jwk).map_err(|_| {
            refused(
                "`signing_key` is not a JWK — it is the project's key as moq.pro gave it, JSON \
                 or base64url JSON"
                    .to_string(),
            )
        })?;
        if !key.operations.contains(&KeyOperation::Sign) {
            return Err(refused(
                "`signing_key` cannot sign: the project's private or symmetric key is what \
                 mints a token"
                    .to_string(),
            ));
        }
        if key.kid.is_none() {
            return Err(refused(
                "`signing_key` carries no `kid`, which moq.pro reads to find the project's key"
                    .to_string(),
            ));
        }
        Ok(Self(key))
    }

    /// A token rooted at the project that publishes `broadcast_path` and
    /// subscribes to anything under the project.
    fn mint_a_token_for_the_broadcast(
        &self,
        project_id: &str,
        broadcast_path: &str,
    ) -> Result<String> {
        let pattern = |pattern: &str| {
            pattern.parse::<Pattern>().map_err(|_| {
                refused(format!("`{pattern}` is not a broadcast pattern a token can grant"))
            })
        };
        let issued_at = SystemTime::now();
        let claims = Claims::default()
            .with_root(project_id)
            .with_publish([pattern(broadcast_path)?])
            .with_subscribe([Pattern::all()])
            .with_issued(issued_at)
            .with_expires(issued_at + MOQPRO_TOKEN_LIFETIME);
        self.0
            .sign(&claims)
            .map_err(|refusal| refused(format!("`signing_key` refused to sign: {refusal}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: Value) -> Result<MoqGatewayRelaySettings> {
        MoqGatewayRelaySettings::read_from(value.as_object().unwrap())
    }

    #[test]
    fn a_moq_relay_is_dialed_at_its_url_with_its_token_in_the_query() {
        let dial = settings(json!({
            "kind": "moq-relay",
            "relay_url": "https://relay.example:4443/anon",
            "token": "t0k",
            "certificate_fingerprint": "ab12",
            "broadcast": "desk",
            "project_id": null,
        }))
        .unwrap()
        .dial_now()
        .unwrap();
        assert_eq!(
            dial.dial_url_carrying_the_credential.as_str(),
            "https://relay.example:4443/anon?jwt=t0k"
        );
        assert_eq!(dial.relay_origin, "https://relay.example:4443");
        assert_eq!(dial.certificate_fingerprints, ["ab12"]);
        assert_eq!(dial.broadcast_path, "desk.hang");
        assert_eq!(dial.catalog_rewritten_whole_every, None);
        assert!(!format!("{dial:?}").contains("t0k"));
    }

    #[test]
    fn a_cloudflare_relay_rewrites_its_catalog_every_second_and_needs_its_token() {
        let dial = settings(json!({
            "kind": "cloudflare",
            "relay_url": "https://relay.cloudflare.example/abcdef",
            "broadcast": "desk",
        }))
        .unwrap()
        .dial_now()
        .unwrap();
        assert_eq!(
            dial.catalog_rewritten_whole_every,
            Some(CLOUDFLARE_CATALOG_IS_REWRITTEN_WHOLE_EVERY)
        );
        let refusal = settings(json!({
            "kind": "cloudflare",
            "relay_url": "https://relay.cloudflare.example/",
            "broadcast": "desk",
        }))
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("carries no token"), "{refusal}");
    }

    #[test]
    fn a_moqpro_project_is_dialed_with_a_token_its_key_signed_and_renewed() {
        let key = Key::generate(
            moq_auth::Algorithm::ES256,
            Some(moq_auth::KeyId::decode("pb1test-default").unwrap()),
        )
        .unwrap();
        let dial = settings(json!({
            "kind": "moqpro",
            "project_id": "pb1test",
            "signing_key": serde_json::to_string(&key).unwrap(),
            "broadcast": "desk",
        }))
        .unwrap()
        .dial_now()
        .unwrap();
        assert!(
            dial.dial_url_carrying_the_credential
                .as_str()
                .starts_with("https://cdn.moq.pro/pb1test?jwt=")
        );
        assert!(dial.renew_by.is_some());
    }

    #[test]
    fn a_key_another_kind_takes_and_an_unknown_kind_are_refused_by_name() {
        let refusal = settings(json!({
            "kind": "cloudflare",
            "relay_url": "https://relay.cloudflare.example/abcdef",
            "token": "t0k",
            "broadcast": "desk",
        }))
        .unwrap_err()
        .to_string();
        assert!(
            refusal.contains("`token` is set") && refusal.contains("`relay_url`"),
            "{refusal}"
        );
        let refusal = settings(json!({"kind": "webrtc", "broadcast": "desk"}))
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("`kind` is `webrtc`"), "{refusal}");
    }

    #[test]
    fn a_broadcast_naming_a_path_and_a_url_that_does_not_parse_are_refused() {
        let refusal = settings(json!({
            "kind": "moq-relay", "relay_url": "http://127.0.0.1:4443", "broadcast": "a/b",
        }))
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("`broadcast` is `a/b`"), "{refusal}");
        let refusal = settings(json!({
            "kind": "moq-relay", "relay_url": "not a url ?jwt=secret", "broadcast": "desk",
        }))
        .unwrap_err()
        .to_string();
        assert!(
            refusal.contains("`relay_url` is not a URL") && !refusal.contains("secret"),
            "{refusal}"
        );
    }
}
