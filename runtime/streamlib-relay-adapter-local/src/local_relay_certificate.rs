// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The certificate a local relay child serves, written by `tatolabd` as files
//! the child reloads when they change.
//!
//! A browser pins a self-signed certificate by its SHA-256 only when it is
//! ECDSA and valid for at most fourteen days (WebTransport's
//! `serverCertificateHashes`), so each is issued for exactly that and replaced
//! before it runs out. X.509 validity is in UTC, so issuing and ageing a
//! certificate reads the wall clock.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::lowercase_hex;

/// How long a certificate is valid, the most a browser pins.
pub const LOCAL_RELAY_CERTIFICATE_VALIDITY: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// How old a certificate is replaced at, leaving days before it runs out.
pub const LOCAL_RELAY_CERTIFICATE_REGENERATION_AGE: Duration =
    Duration::from_secs(10 * 24 * 60 * 60);

/// How far before its issue a certificate is already valid, so a reader whose
/// clock runs slightly behind still accepts it.
const LOCAL_RELAY_CERTIFICATE_BACKDATING: Duration = Duration::from_secs(60 * 60);

/// Only the owner reads or writes a certificate or its key.
const OWNER_ONLY_FILE_MODE: u32 = 0o600;

/// What a fresh file is written as before it is renamed into place, so the
/// child's watcher only ever sees a whole file.
const NEXT_FILE_SUFFIX: &str = "next";

/// The one file a child's certificate and its key live in, so a rotation is
/// one rename and the child's watcher never pairs a new key with an old
/// certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRelayCertificateAndKeyFile {
    /// The certificate, then its PKCS#8 private key, both PEM.
    pub certificate_and_key_path: PathBuf,
}

/// A certificate written to its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedLocalRelayCertificate {
    /// When it was issued, as the wall clock read then.
    pub issued_at: SystemTime,
    /// The SHA-256 of its DER, lowercase hex: what the child serves at
    /// `/certificate.sha256`.
    pub sha256_fingerprint_hex: String,
}

/// Why a certificate could not be provisioned.
#[derive(Debug, thiserror::Error)]
pub enum LocalRelayCertificateRefusal {
    #[error("the local relay's certificate could not be generated: {0}")]
    NotGenerated(#[from] rcgen::Error),
    #[error("the local relay's certificate could not be written to {}: {source}", path.display())]
    NotWritten {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl LocalRelayCertificateAndKeyFile {
    /// The certificate-and-key file inside `directory`.
    pub fn in_the_directory(directory: &Path) -> Self {
        Self {
            certificate_and_key_path: directory.join("certificate-and-key.pem"),
        }
    }

    /// Issue a fresh ECDSA P-256 certificate for `subject_alt_names` now and
    /// replace the file with it and its key.
    pub fn write_a_fresh_certificate_issued_now(
        &self,
        subject_alt_names: &[String],
    ) -> Result<ProvisionedLocalRelayCertificate, LocalRelayCertificateRefusal> {
        self.write_a_fresh_certificate(subject_alt_names, SystemTime::now())
    }

    /// Issue a fresh ECDSA P-256 certificate for `subject_alt_names` at
    /// `issued_at` and replace the file with it and its key.
    fn write_a_fresh_certificate(
        &self,
        subject_alt_names: &[String],
        issued_at: SystemTime,
    ) -> Result<ProvisionedLocalRelayCertificate, LocalRelayCertificateRefusal> {
        let private_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let mut parameters = rcgen::CertificateParams::new(subject_alt_names.to_vec())?;
        let not_before = issued_at
            .checked_sub(LOCAL_RELAY_CERTIFICATE_BACKDATING)
            .unwrap_or(issued_at);
        parameters.not_before = time::OffsetDateTime::from(not_before);
        parameters.not_after =
            time::OffsetDateTime::from(not_before + LOCAL_RELAY_CERTIFICATE_VALIDITY);
        let certificate = parameters.self_signed(&private_key)?;

        let certificate_and_key_pem =
            format!("{}{}", certificate.pem(), private_key.serialize_pem());
        replace_the_file_whole(
            &self.certificate_and_key_path,
            certificate_and_key_pem.as_bytes(),
        )?;
        Ok(ProvisionedLocalRelayCertificate {
            issued_at,
            sha256_fingerprint_hex: sha256_hex(certificate.der()),
        })
    }
}

impl ProvisionedLocalRelayCertificate {
    /// Whether this certificate is old enough now to be replaced.
    pub fn is_due_for_regeneration_now(&self) -> bool {
        self.is_due_for_regeneration_at(SystemTime::now())
    }

    /// Whether this certificate is old enough at `now` to be replaced.
    fn is_due_for_regeneration_at(&self, now: SystemTime) -> bool {
        now.duration_since(self.issued_at)
            .is_ok_and(|age| age >= LOCAL_RELAY_CERTIFICATE_REGENERATION_AGE)
    }
}

fn replace_the_file_whole(
    path: &Path,
    contents: &[u8],
) -> Result<(), LocalRelayCertificateRefusal> {
    let next_path = path.with_extension(NEXT_FILE_SUFFIX);
    let not_written = |source| LocalRelayCertificateRefusal::NotWritten {
        path: path.to_path_buf(),
        source,
    };
    let mut next_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(OWNER_ONLY_FILE_MODE)
        .open(&next_path)
        .map_err(not_written)?;
    next_file.write_all(contents).map_err(not_written)?;
    next_file.sync_all().map_err(not_written)?;
    std::fs::rename(&next_path, path).map_err(not_written)
}

fn sha256_hex(bytes: &[u8]) -> String {
    lowercase_hex(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn subject_alt_names() -> Vec<String> {
        vec!["localhost".to_string(), "127.0.0.1".to_string()]
    }

    #[test]
    fn the_certificate_and_its_key_are_written_to_one_owner_only_file_and_fingerprinted() {
        let directory = tempfile::tempdir().unwrap();
        let file = LocalRelayCertificateAndKeyFile::in_the_directory(directory.path());
        let provisioned = file
            .write_a_fresh_certificate(&subject_alt_names(), SystemTime::now())
            .unwrap();

        let certificate_and_key_pem =
            std::fs::read_to_string(&file.certificate_and_key_path).unwrap();
        assert!(certificate_and_key_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(certificate_and_key_pem.contains("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(
            std::fs::metadata(&file.certificate_and_key_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            OWNER_ONLY_FILE_MODE
        );

        let der = pem_body_as_der(&certificate_and_key_pem);
        assert_eq!(provisioned.sha256_fingerprint_hex, sha256_hex(&der));
        assert_eq!(provisioned.sha256_fingerprint_hex.len(), 64);
    }

    #[test]
    fn the_certificate_is_ecdsa_p256_and_valid_for_fourteen_days() {
        let directory = tempfile::tempdir().unwrap();
        let file = LocalRelayCertificateAndKeyFile::in_the_directory(directory.path());
        file.write_a_fresh_certificate(&subject_alt_names(), SystemTime::now())
            .unwrap();
        let der =
            pem_body_as_der(&std::fs::read_to_string(&file.certificate_and_key_path).unwrap());

        const ECDSA_WITH_SHA256_OID_DER: &[u8] =
            &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
        const PRIME256V1_OID_DER: &[u8] =
            &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
        assert!(
            contains(&der, ECDSA_WITH_SHA256_OID_DER),
            "signed ecdsa-with-SHA256"
        );
        assert!(contains(&der, PRIME256V1_OID_DER), "a P-256 key");

        let (not_before, not_after) = validity_of(&der);
        assert_eq!(
            not_after.duration_since(not_before).unwrap(),
            LOCAL_RELAY_CERTIFICATE_VALIDITY
        );
    }

    #[test]
    fn writing_again_replaces_the_file_with_a_new_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let file = LocalRelayCertificateAndKeyFile::in_the_directory(directory.path());
        let first = file
            .write_a_fresh_certificate(&subject_alt_names(), SystemTime::now())
            .unwrap();
        let second = file
            .write_a_fresh_certificate(&subject_alt_names(), SystemTime::now())
            .unwrap();
        assert_ne!(first.sha256_fingerprint_hex, second.sha256_fingerprint_hex);
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            1,
            "no `.next` file is left beside it"
        );
    }

    #[test]
    fn a_certificate_is_due_for_regeneration_from_its_tenth_day() {
        let issued_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let provisioned = ProvisionedLocalRelayCertificate {
            issued_at,
            sha256_fingerprint_hex: String::new(),
        };
        let one_second = Duration::from_secs(1);
        assert!(!provisioned.is_due_for_regeneration_at(issued_at));
        assert!(!provisioned.is_due_for_regeneration_at(
            issued_at + LOCAL_RELAY_CERTIFICATE_REGENERATION_AGE - one_second
        ));
        assert!(
            provisioned
                .is_due_for_regeneration_at(issued_at + LOCAL_RELAY_CERTIFICATE_REGENERATION_AGE)
        );
        assert!(
            !provisioned.is_due_for_regeneration_at(issued_at - one_second),
            "a clock set back never ages a certificate"
        );
    }

    fn pem_body_as_der(pem: &str) -> Vec<u8> {
        let body: String = pem
            .lines()
            .skip(1)
            .take_while(|line| !line.starts_with("-----"))
            .collect();
        decode_base64(&body)
    }

    fn decode_base64(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits: u32 = 0;
        let mut bit_count = 0;
        let mut decoded = Vec::new();
        for byte in text.bytes().filter(|byte| *byte != b'=') {
            let value = ALPHABET.iter().position(|letter| *letter == byte).unwrap() as u32;
            bits = (bits << 6) | value;
            bit_count += 6;
            if bit_count >= 8 {
                bit_count -= 8;
                decoded.push((bits >> bit_count) as u8);
                bits &= (1 << bit_count) - 1;
            }
        }
        decoded
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// The two `UTCTime`s of the certificate's validity, in order.
    fn validity_of(der: &[u8]) -> (SystemTime, SystemTime) {
        const UTC_TIME_TAG: u8 = 0x17;
        const UTC_TIME_LENGTH: usize = 13;
        let times: Vec<SystemTime> = der
            .windows(2 + UTC_TIME_LENGTH)
            .filter(|window| window[0] == UTC_TIME_TAG && window[1] as usize == UTC_TIME_LENGTH)
            .map(|window| utc_time(&window[2..]))
            .take(2)
            .collect();
        (times[0], times[1])
    }

    /// `YYMMDDHHMMSSZ` as a time.
    fn utc_time(text: &[u8]) -> SystemTime {
        let field = |at: usize| -> u8 {
            std::str::from_utf8(&text[at..at + 2])
                .unwrap()
                .parse()
                .unwrap()
        };
        let date = time::Date::from_calendar_date(
            2000 + i32::from(field(0)),
            time::Month::try_from(field(2)).unwrap(),
            field(4),
        )
        .unwrap();
        let date_time = date
            .with_hms(field(6), field(8), field(10))
            .unwrap()
            .assume_utc();
        SystemTime::from(date_time)
    }
}
