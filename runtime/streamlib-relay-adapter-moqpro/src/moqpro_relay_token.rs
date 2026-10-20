// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The JWTs a moq.pro relay admits: signed with the project's key, rooted at
//! the project, and expiring at a UTC time the relay checks against its own
//! clock.

use std::time::{Duration, Instant, SystemTime};

use moq_auth::{Claims, Key, KeyOperation, Pattern};
use streamlib_relay_adapter_sdk::secrecy::SecretString;
use streamlib_relay_adapter_sdk::{MachineName, RelayDialCredential};

use crate::moqpro_relay_instance_settings::MoqProProjectId;

/// How long a publish token the adapter mints is valid.
pub const MOQPRO_PUBLISH_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// How long after minting a publish token the runtime dials with a fresh one,
/// well inside [`MOQPRO_PUBLISH_TOKEN_LIFETIME`]: the relay ends a session
/// whose token has expired.
pub const MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER: Duration = Duration::from_secs(20 * 60 * 60);

/// Why a moq.pro signing key or token was refused. It never carries the key's
/// material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoqProSigningKeyRefusal {
    /// Not a JWK, as JSON or base64url JSON.
    NotAJwk,
    /// A public key, or one restricted from signing.
    CannotSign,
    /// No `kid`, which moq.pro reads to find the project the key belongs to.
    NoKeyId,
    /// A pattern the token would grant does not parse.
    PatternRefused { pattern: String },
    /// The key refused to sign the claims.
    SigningRefused { why: String },
}

impl std::fmt::Display for MoqProSigningKeyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAJwk => f.write_str(
                "`signing_key` is not a JWK — it is the project's key as moq.pro gave it, JSON or \
                 base64url JSON",
            ),
            Self::CannotSign => f.write_str(
                "`signing_key` cannot sign: it is a public key or restricted from signing, and \
                 the project's private or symmetric key is what mints a token",
            ),
            Self::NoKeyId => f.write_str(
                "`signing_key` carries no `kid`, which moq.pro reads to find the project's key",
            ),
            Self::PatternRefused { pattern } => {
                write!(
                    f,
                    "`{pattern}` is not a broadcast pattern a token can grant"
                )
            }
            Self::SigningRefused { why } => write!(f, "`signing_key` refused to sign: {why}"),
        }
    }
}

/// A project's signing key, read and checked able to mint a token moq.pro
/// admits.
#[derive(Clone)]
pub struct MoqProProjectSigningKey(Key);

impl std::fmt::Debug for MoqProProjectSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MoqProProjectSigningKey([REDACTED])")
    }
}

impl MoqProProjectSigningKey {
    /// `signing_key` read as the project's key, JSON or base64url JSON.
    pub fn read_from(signing_key: &str) -> Result<Self, MoqProSigningKeyRefusal> {
        let key = Key::from_str(signing_key).map_err(|_| MoqProSigningKeyRefusal::NotAJwk)?;
        if !key.operations.contains(&KeyOperation::Sign) {
            return Err(MoqProSigningKeyRefusal::CannotSign);
        }
        if key.kid.is_none() {
            return Err(MoqProSigningKeyRefusal::NoKeyId);
        }
        Ok(Self(key))
    }

    /// The credential a dial presents: a token minted now, rooted at
    /// `project_id`, granting publish to `machine`'s own broadcasts, valid for
    /// [`MOQPRO_PUBLISH_TOKEN_LIFETIME`] and renewed after
    /// [`MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER`].
    pub fn mint_a_publish_credential_for_the_machine(
        &self,
        project_id: &MoqProProjectId,
        machine: &MachineName,
    ) -> Result<RelayDialCredential, MoqProSigningKeyRefusal> {
        let issued_at = SystemTime::now();
        let renew_by = Instant::now() + MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER;
        let claims = Claims::default()
            .with_root(project_id.as_str())
            .with_publish([broadcast_pattern(&format!("{}/**", machine.name()))?])
            .with_issued(issued_at)
            .with_expires(issued_at + MOQPRO_PUBLISH_TOKEN_LIFETIME);
        Ok(
            RelayDialCredential::token_in_the_dial_url_query(self.sign(&claims)?)
                .renewed_by(renew_by),
        )
    }

    /// A token rooted at `project_id` granting subscribe to `broadcast` alone,
    /// valid for `lifetime` from now.
    pub fn mint_a_subscribe_only_token(
        &self,
        project_id: &MoqProProjectId,
        broadcast: &str,
        lifetime: Duration,
    ) -> Result<SecretString, MoqProSigningKeyRefusal> {
        let issued_at = SystemTime::now();
        let claims = Claims::default()
            .with_root(project_id.as_str())
            .with_subscribe([broadcast_pattern(broadcast)?])
            .with_issued(issued_at)
            .with_expires(issued_at + lifetime);
        self.sign(&claims)
    }

    fn sign(&self, claims: &Claims) -> Result<SecretString, MoqProSigningKeyRefusal> {
        self.0
            .sign(claims)
            .map(SecretString::from)
            .map_err(|refusal| MoqProSigningKeyRefusal::SigningRefused {
                why: refusal.to_string(),
            })
    }
}

fn broadcast_pattern(pattern: &str) -> Result<Pattern, MoqProSigningKeyRefusal> {
    pattern
        .parse()
        .map_err(|_| MoqProSigningKeyRefusal::PatternRefused {
            pattern: pattern.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use moq_auth::{Algorithm, KeyId};
    use streamlib_relay_adapter_sdk::secrecy::ExposeSecret;
    use streamlib_relay_adapter_sdk::url::Url;

    fn the_project() -> MoqProProjectId {
        MoqProProjectId::try_from("pb1test".to_string()).unwrap()
    }

    /// The token `credential` carries in a dial URL's query.
    fn the_token_of(credential: &RelayDialCredential) -> String {
        let dial_url = credential.dial_url_carrying_the_credential(
            &Url::parse("https://relay.example/pb1test").unwrap(),
        );
        Url::parse(dial_url.expose_secret())
            .unwrap()
            .query_pairs()
            .find(|(parameter, _)| parameter == "jwt")
            .unwrap()
            .1
            .into_owned()
    }

    fn a_project_key(algorithm: Algorithm) -> Key {
        Key::generate(algorithm, Some(KeyId::decode("pb1test-default").unwrap())).unwrap()
    }

    fn the_jwk_json_of(key: &Key) -> String {
        serde_json::to_string(key).unwrap()
    }

    fn the_header_of(token: &str) -> serde_json::Value {
        let header = token.split('.').next().unwrap();
        serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(header)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_publish_token_is_rooted_at_the_project_and_grants_the_machines_broadcasts_for_a_day() {
        for algorithm in [Algorithm::ES256, Algorithm::HS256] {
            let key = a_project_key(algorithm);
            let signing_key = MoqProProjectSigningKey::read_from(&the_jwk_json_of(&key)).unwrap();
            let credential = signing_key
                .mint_a_publish_credential_for_the_machine(
                    &the_project(),
                    &MachineName::from_a_hostname("Desk").unwrap(),
                )
                .unwrap();
            let token = the_token_of(&credential);
            let claims = key.verify(&token).unwrap();
            assert_eq!(claims.root, "pb1test");
            assert_eq!(
                claims.publish,
                ["desk/**".parse::<Pattern>().unwrap()]
                    .into_iter()
                    .collect()
            );
            assert!(claims.subscribe.is_empty());
            let lifetime = claims
                .expires
                .unwrap()
                .duration_since(claims.issued.unwrap())
                .unwrap();
            assert_eq!(lifetime, MOQPRO_PUBLISH_TOKEN_LIFETIME);
            let header = the_header_of(&token);
            assert_eq!(header["kid"], "pb1test-default");
            assert_eq!(header["alg"], format!("{algorithm:?}"));
            assert!(
                claims
                    .authorize("pb1test")
                    .unwrap()
                    .publish
                    .iter()
                    .eq(["desk/**".parse::<Pattern>().unwrap()].iter()),
                "dialed at the project's path, the grant reads the machine's broadcasts under it"
            );
        }
    }

    #[test]
    fn a_publish_credential_is_renewed_well_before_its_token_expires() {
        let margin = MOQPRO_PUBLISH_TOKEN_LIFETIME - MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER;
        assert!(margin >= Duration::from_secs(60 * 60), "{margin:?}");
        let key = a_project_key(Algorithm::ES256);
        let minted_after = Instant::now();
        let renew_by = MoqProProjectSigningKey::read_from(&the_jwk_json_of(&key))
            .unwrap()
            .mint_a_publish_credential_for_the_machine(
                &the_project(),
                &MachineName::from_a_hostname("desk").unwrap(),
            )
            .unwrap()
            .renew_by()
            .unwrap();
        assert!(renew_by >= minted_after + MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER);
        assert!(renew_by <= Instant::now() + MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER);
    }

    #[test]
    fn a_subscribe_only_token_grants_the_one_broadcast_and_no_publish() {
        let key = a_project_key(Algorithm::ES256);
        let signing_key = MoqProProjectSigningKey::read_from(&the_jwk_json_of(&key)).unwrap();
        let token = signing_key
            .mint_a_subscribe_only_token(
                &the_project(),
                "desk/sample.hang",
                Duration::from_secs(600),
            )
            .unwrap();
        let claims = key.verify(token.expose_secret()).unwrap();
        assert_eq!(claims.root, "pb1test");
        assert!(claims.publish.is_empty());
        assert_eq!(
            claims.subscribe,
            ["desk/sample.hang".parse::<Pattern>().unwrap()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn a_key_moq_pro_cannot_use_is_refused_without_its_material() {
        assert_eq!(
            MoqProProjectSigningKey::read_from("{not a jwk").unwrap_err(),
            MoqProSigningKeyRefusal::NotAJwk
        );
        let public_key = a_project_key(Algorithm::ES256).to_public().unwrap();
        assert_eq!(
            MoqProProjectSigningKey::read_from(&the_jwk_json_of(&public_key)).unwrap_err(),
            MoqProSigningKeyRefusal::CannotSign
        );
        let without_a_kid = Key::generate(Algorithm::ES256, None).unwrap();
        assert_eq!(
            MoqProProjectSigningKey::read_from(&the_jwk_json_of(&without_a_kid)).unwrap_err(),
            MoqProSigningKeyRefusal::NoKeyId
        );
    }

    #[test]
    fn a_key_in_base64url_json_is_read_as_the_json_is() {
        let key = a_project_key(Algorithm::HS256);
        assert!(MoqProProjectSigningKey::read_from(&key.to_str().unwrap()).is_ok());
    }

    #[test]
    fn a_signing_key_never_prints_its_material() {
        let key = a_project_key(Algorithm::HS256);
        let jwk_json = the_jwk_json_of(&key);
        let secret = serde_json::from_str::<serde_json::Value>(&jwk_json).unwrap()["k"]
            .as_str()
            .unwrap()
            .to_string();
        let signing_key = MoqProProjectSigningKey::read_from(&jwk_json).unwrap();
        assert!(!format!("{signing_key:?}").contains(&secret));
    }
}
