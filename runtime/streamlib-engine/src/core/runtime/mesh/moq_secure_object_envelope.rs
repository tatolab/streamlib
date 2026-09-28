// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The secure object envelope (v1) every MoQ gateway object rides in once a
//! content key is configured.
//!
//! Wire contract, shared with every subscriber that opens it:
//! `b"SLE1"` ‖ key epoch (u32 big-endian) ‖ 12-byte random IV ‖
//! AES-128-GCM ciphertext with its 16-byte tag appended. The additional
//! authenticated data is the envelope's first eight bytes — magic and epoch —
//! so a relay that rewrites the epoch to steer a subscriber onto another key fails
//! the tag rather than decrypting under the wrong key.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};

/// The four ASCII bytes every sealed object begins with.
pub(crate) const SECURE_OBJECT_ENVELOPE_MAGIC: &[u8; 4] = b"SLE1";

/// Bytes ahead of the ciphertext: magic, epoch, IV.
const SECURE_OBJECT_ENVELOPE_HEADER_BYTES: usize = 4 + 4 + 12;

/// The AES-GCM tag's length.
const AES_GCM_TAG_BYTES: usize = 16;

/// How many leading envelope bytes are authenticated but not encrypted.
const AUTHENTICATED_HEADER_BYTES: usize = 8;

/// One content key a track is sealed with, and the epoch that names it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct MoqGatewayContentKey {
    pub(crate) epoch: u32,
    pub(crate) key_bytes: [u8; 16],
}

impl std::fmt::Debug for MoqGatewayContentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key bytes never reach a log line.
        f.debug_struct("MoqGatewayContentKey")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// Why a sealed object could not be opened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WhyASecureObjectDidNotOpen {
    ItIsShorterThanAnEnvelope { envelope_bytes: usize },
    ItDoesNotBeginWithTheMagic,
    NoKeyIsConfiguredForItsEpoch { epoch: u32 },
    ItsTagDidNotVerify { epoch: u32 },
}

impl std::fmt::Display for WhyASecureObjectDidNotOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ItIsShorterThanAnEnvelope { envelope_bytes } => write!(
                f,
                "it is {envelope_bytes} bytes, shorter than an empty sealed object"
            ),
            Self::ItDoesNotBeginWithTheMagic => write!(f, "it does not begin with SLE1"),
            Self::NoKeyIsConfiguredForItsEpoch { epoch } => {
                write!(f, "no content key is configured for its epoch {epoch}")
            }
            Self::ItsTagDidNotVerify { epoch } => write!(
                f,
                "its AES-GCM tag did not verify under the epoch {epoch} key"
            ),
        }
    }
}

/// Whether `object_bytes` begins the way a sealed object does.
pub(crate) fn a_moq_object_is_sealed(object_bytes: &[u8]) -> bool {
    object_bytes.starts_with(SECURE_OBJECT_ENVELOPE_MAGIC)
}

/// Seal `plaintext_object` under `content_key` with the IV given.
///
/// The IV must never repeat under one key; [`seal_a_moq_object_with_a_fresh_iv`]
/// is the only caller outside the tests, which pin a fixed vector.
pub(crate) fn seal_a_moq_object_with_this_iv(
    content_key: &MoqGatewayContentKey,
    initialization_vector: [u8; 12],
    plaintext_object: &[u8],
) -> Result<Vec<u8>, String> {
    let mut envelope = Vec::with_capacity(
        SECURE_OBJECT_ENVELOPE_HEADER_BYTES + plaintext_object.len() + AES_GCM_TAG_BYTES,
    );
    envelope.extend_from_slice(SECURE_OBJECT_ENVELOPE_MAGIC);
    envelope.extend_from_slice(&content_key.epoch.to_be_bytes());
    envelope.extend_from_slice(&initialization_vector);
    let cipher = Aes128Gcm::new_from_slice(&content_key.key_bytes)
        .map_err(|wrong_length| format!("the content key is not 16 bytes: {wrong_length}"))?;
    let ciphertext_and_tag = cipher
        .encrypt(
            Nonce::from_slice(&initialization_vector),
            Payload {
                msg: plaintext_object,
                aad: &envelope[..AUTHENTICATED_HEADER_BYTES],
            },
        )
        .map_err(|_| "AES-GCM refused to seal the object".to_string())?;
    envelope.extend_from_slice(&ciphertext_and_tag);
    Ok(envelope)
}

/// Seal `plaintext_object` under `content_key` with a random IV from the OS.
pub(crate) fn seal_a_moq_object_with_a_fresh_iv(
    content_key: &MoqGatewayContentKey,
    plaintext_object: &[u8],
) -> Result<Vec<u8>, String> {
    let mut initialization_vector = [0u8; 12];
    getrandom::getrandom(&mut initialization_vector)
        .map_err(|no_randomness| format!("the OS gave no random IV: {no_randomness}"))?;
    seal_a_moq_object_with_this_iv(content_key, initialization_vector, plaintext_object)
}

/// Open a sealed object with whichever of `content_keys` its epoch names.
pub(crate) fn open_a_sealed_moq_object(
    content_keys: &[MoqGatewayContentKey],
    envelope: &[u8],
) -> Result<Vec<u8>, WhyASecureObjectDidNotOpen> {
    if envelope.len() < SECURE_OBJECT_ENVELOPE_HEADER_BYTES + AES_GCM_TAG_BYTES {
        return Err(WhyASecureObjectDidNotOpen::ItIsShorterThanAnEnvelope {
            envelope_bytes: envelope.len(),
        });
    }
    if !a_moq_object_is_sealed(envelope) {
        return Err(WhyASecureObjectDidNotOpen::ItDoesNotBeginWithTheMagic);
    }
    let epoch = u32::from_be_bytes([envelope[4], envelope[5], envelope[6], envelope[7]]);
    let content_key = content_keys
        .iter()
        .find(|content_key| content_key.epoch == epoch)
        .ok_or(WhyASecureObjectDidNotOpen::NoKeyIsConfiguredForItsEpoch { epoch })?;
    let cipher = Aes128Gcm::new_from_slice(&content_key.key_bytes)
        .map_err(|_| WhyASecureObjectDidNotOpen::ItsTagDidNotVerify { epoch })?;
    cipher
        .decrypt(
            Nonce::from_slice(
                &envelope[AUTHENTICATED_HEADER_BYTES..SECURE_OBJECT_ENVELOPE_HEADER_BYTES],
            ),
            Payload {
                msg: &envelope[SECURE_OBJECT_ENVELOPE_HEADER_BYTES..],
                aad: &envelope[..AUTHENTICATED_HEADER_BYTES],
            },
        )
        .map_err(|_| WhyASecureObjectDidNotOpen::ItsTagDidNotVerify { epoch })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn the_vector_key() -> MoqGatewayContentKey {
        MoqGatewayContentKey {
            epoch: 3,
            key_bytes: std::array::from_fn(|index| index as u8),
        }
    }

    fn the_vector_iv() -> [u8; 12] {
        std::array::from_fn(|index| 0xa0 + index as u8)
    }

    /// Produced independently with Python's `cryptography` AESGCM over the
    /// same key, IV, epoch and plaintext — the bytes a subscriber must open.
    const THE_VECTOR_ENVELOPE_HEX: &str = "534c453100000003a0a1a2a3a4a5a6a7a8a9aaabd9f24ade1fe45f63e858d86f3732d701278b5469fa96216ab60b9d318fff81cdd0385b8a84";

    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn a_fixed_iv_seals_to_the_independently_computed_vector() {
        let sealed = seal_a_moq_object_with_this_iv(
            &the_vector_key(),
            the_vector_iv(),
            b"streamlib moq gateway",
        )
        .expect("the object seals");
        assert_eq!(hex_of(&sealed), THE_VECTOR_ENVELOPE_HEX);
    }

    #[test]
    fn a_sealed_object_opens_back_to_its_plaintext() {
        let plaintext = b"an attachment and a bag".repeat(40);
        let sealed = seal_a_moq_object_with_a_fresh_iv(&the_vector_key(), &plaintext)
            .expect("the object seals");
        assert!(a_moq_object_is_sealed(&sealed));
        assert_eq!(
            open_a_sealed_moq_object(&[the_vector_key()], &sealed),
            Ok(plaintext)
        );
    }

    #[test]
    fn two_seals_of_one_object_never_share_an_iv() {
        let first = seal_a_moq_object_with_a_fresh_iv(&the_vector_key(), b"same").unwrap();
        let second = seal_a_moq_object_with_a_fresh_iv(&the_vector_key(), b"same").unwrap();
        assert_ne!(first[8..20], second[8..20]);
    }

    #[test]
    fn an_object_opens_with_the_key_its_epoch_names_among_several() {
        let older = MoqGatewayContentKey {
            epoch: 2,
            key_bytes: [7; 16],
        };
        let sealed = seal_a_moq_object_with_a_fresh_iv(&older, b"under epoch two").unwrap();
        assert_eq!(
            open_a_sealed_moq_object(&[the_vector_key(), older], &sealed),
            Ok(b"under epoch two".to_vec())
        );
    }

    #[test]
    fn a_rewritten_epoch_fails_the_tag_rather_than_opening_under_another_key() {
        let same_key_other_epoch = MoqGatewayContentKey {
            epoch: 4,
            ..the_vector_key()
        };
        let mut sealed =
            seal_a_moq_object_with_this_iv(&the_vector_key(), the_vector_iv(), b"x").unwrap();
        sealed[7] = 4;
        assert_eq!(
            open_a_sealed_moq_object(&[same_key_other_epoch], &sealed),
            Err(WhyASecureObjectDidNotOpen::ItsTagDidNotVerify { epoch: 4 })
        );
    }

    #[test]
    fn an_object_whose_epoch_has_no_key_names_the_epoch() {
        let sealed =
            seal_a_moq_object_with_this_iv(&the_vector_key(), the_vector_iv(), b"x").unwrap();
        assert_eq!(
            open_a_sealed_moq_object(&[], &sealed),
            Err(WhyASecureObjectDidNotOpen::NoKeyIsConfiguredForItsEpoch { epoch: 3 })
        );
    }

    #[test]
    fn bytes_that_are_not_an_envelope_are_refused_by_name() {
        assert_eq!(
            open_a_sealed_moq_object(&[the_vector_key()], b"SLE1"),
            Err(WhyASecureObjectDidNotOpen::ItIsShorterThanAnEnvelope { envelope_bytes: 4 })
        );
        assert_eq!(
            open_a_sealed_moq_object(&[the_vector_key()], &[0u8; 64]),
            Err(WhyASecureObjectDidNotOpen::ItDoesNotBeginWithTheMagic)
        );
    }

    #[test]
    fn a_content_keys_debug_rendering_never_shows_its_bytes() {
        let rendered = format!("{:?}", the_vector_key());
        assert!(!rendered.contains("key_bytes"), "{rendered}");
    }
}
