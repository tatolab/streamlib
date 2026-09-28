// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Spike-only test subscriber for the MoQ gateway.
//!
//! Subscribes to one track on a relay, and for every object checks the secure
//! object envelope (`SLE1` ‖ epoch BE ‖ IV ‖ AES-128-GCM with the first eight
//! bytes as AAD), opens it with the key given, splits the plaintext into the
//! attachment-length prefix, the 44-byte attachment and the msgpack bag, and
//! prints one JSON line per object and a summary. Written against the wire
//! contract, sharing no code with the engine.
//!
//!   MOQ_TEST_RELAY_URL=https://relay/<token> moq-gateway-test-subscriber \
//!     --namespace example/spike/rt/1727000000 --track "Ticker/out" \
//!     [--key-base64url AAEC...] [--count 20] [--accept-any-certificate]
//!
//! The relay URL is read from the environment so its token never rides argv.

#![allow(clippy::disallowed_macros)] // a CLI: stdout is its output channel

use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;

struct Arguments {
    namespace: String,
    track: String,
    key: Option<[u8; 16]>,
    count: usize,
    accept_any_certificate: bool,
    timeout_seconds: u64,
}

fn read_arguments() -> Result<Arguments, String> {
    let mut arguments = Arguments {
        namespace: String::new(),
        track: String::new(),
        key: None,
        count: 20,
        accept_any_certificate: false,
        timeout_seconds: 30,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        match flag.as_str() {
            "--namespace" => arguments.namespace = raw.next().ok_or("--namespace needs a value")?,
            "--track" => arguments.track = raw.next().ok_or("--track needs a value")?,
            "--count" => {
                arguments.count = raw
                    .next()
                    .ok_or("--count needs a value")?
                    .parse()
                    .map_err(|_| "--count is a number")?
            }
            "--timeout-seconds" => {
                arguments.timeout_seconds = raw
                    .next()
                    .ok_or("--timeout-seconds needs a value")?
                    .parse()
                    .map_err(|_| "--timeout-seconds is a number")?
            }
            "--key-base64url" => {
                let encoded = raw.next().ok_or("--key-base64url needs a value")?;
                let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(encoded.trim_end_matches('='))
                    .map_err(|_| "the key is not base64url")?;
                arguments.key = Some(decoded.try_into().map_err(|_| "the key is not 16 bytes")?);
            }
            "--accept-any-certificate" => arguments.accept_any_certificate = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if arguments.namespace.is_empty() || arguments.track.is_empty() {
        return Err("--namespace and --track are required".to_string());
    }
    Ok(arguments)
}

#[derive(Debug)]
struct AcceptsAnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptsAnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, certificate, signature, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, certificate, signature, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

async fn connect(relay_url: &str, accept_any_certificate: bool) -> Result<moq_transport::session::Subscriber, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = web_transport::quinn::crypto::default_provider();
    let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?;
    let mut crypto = if accept_any_certificate {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptsAnyCertificate(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in rustls_native_certs::load_native_certs().certs {
            let _ = roots.add(certificate);
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    crypto.alpn_protocols = vec![web_transport::quinn::ALPN.as_bytes().to_vec()];
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(crypto).map_err(|e| e.to_string())?;
    let client_config = quinn::ClientConfig::new(Arc::new(quic));
    let endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())
        .or_else(|_| quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()))
        .map_err(|e| e.to_string())?;
    let url = url::Url::parse(relay_url).map_err(|_| "the relay URL does not parse".to_string())?;
    let request = web_transport::quinn::proto::ConnectRequest::new(url)
        .with_protocol(std::str::from_utf8(moq_transport::setup::ALPN).unwrap());
    let session = web_transport::quinn::Client::new(endpoint, client_config)
        .connect(request)
        .await
        .map_err(|e| format!("the relay refused the session: {e}"))?;
    let (session, _publisher, subscriber) = moq_transport::session::Session::connect(
        session.into(),
        None,
        moq_transport::session::Transport::WebTransport,
    )
    .await
    .map_err(|e| format!("the MoQ session did not open: {e}"))?;
    tokio::spawn(async move {
        let _ = session.run().await;
    });
    Ok(subscriber)
}

/// One object, checked against the wire contract.
fn describe(object: &[u8], key: Option<&[u8; 16]>) -> serde_json::Value {
    let mut described = serde_json::json!({ "object_bytes": object.len() });
    let sealed = object.starts_with(b"SLE1");
    described["sealed"] = sealed.into();
    let plaintext: Vec<u8> = if sealed {
        if object.len() < 36 {
            described["error"] = "shorter than an envelope".into();
            return described;
        }
        let epoch = u32::from_be_bytes([object[4], object[5], object[6], object[7]]);
        described["key_epoch"] = epoch.into();
        let Some(key) = key else {
            described["error"] = "sealed and no key given".into();
            return described;
        };
        let cipher = Aes128Gcm::new_from_slice(key).unwrap();
        match cipher.decrypt(Nonce::from_slice(&object[8..20]), Payload { msg: &object[20..], aad: &object[..8] }) {
            Ok(plaintext) => {
                described["decrypted"] = true.into();
                plaintext
            }
            Err(_) => {
                described["decrypted"] = false.into();
                described["error"] = "the tag did not verify".into();
                return described;
            }
        }
    } else {
        object.to_vec()
    };
    if plaintext.len() < 4 {
        described["error"] = "no attachment length".into();
        return described;
    }
    let attachment_length = u32::from_le_bytes(plaintext[..4].try_into().unwrap()) as usize;
    described["attachment_length"] = attachment_length.into();
    if plaintext.len() < 4 + attachment_length || attachment_length < 44 {
        described["error"] = "the attachment is truncated".into();
        return described;
    }
    let attachment = &plaintext[4..4 + attachment_length];
    described["timestamp_ns"] = i64::from_le_bytes(attachment[0..8].try_into().unwrap()).into();
    described["sequence_number"] = u64::from_le_bytes(attachment[8..16].try_into().unwrap()).into();
    described["publisher_generation"] = u64::from_le_bytes(attachment[16..24].try_into().unwrap()).into();
    let frame_pixel_description_bytes = u32::from_le_bytes(attachment[40..44].try_into().unwrap());
    described["frame_pixel_description_bytes"] = frame_pixel_description_bytes.into();
    let payload = &plaintext[4 + attachment_length..];
    described["payload_bytes"] = payload.len().into();
    if frame_pixel_description_bytes == 0 {
        match rmpv::decode::read_value(&mut &payload[..]) {
            Ok(rmpv::Value::Map(entries)) => {
                let keys: Vec<String> = entries
                    .iter()
                    .map(|(key, value)| match (key.as_str(), value) {
                        (Some(key), rmpv::Value::Binary(bytes)) => format!("{key}=<bin {}>", bytes.len()),
                        (Some(key), rmpv::Value::String(text)) => format!("{key}={}", text.as_str().unwrap_or("?")),
                        (Some(key), rmpv::Value::Integer(number)) => format!("{key}={number}"),
                        (Some(key), rmpv::Value::Boolean(flag)) => format!("{key}={flag}"),
                        (Some(key), _) => key.to_string(),
                        (None, _) => "<non-string key>".to_string(),
                    })
                    .collect();
                described["bag_keys"] = keys.into();
            }
            Ok(_) => described["error"] = "the payload is msgpack but not a map".into(),
            Err(_) => described["error"] = "the payload is not msgpack".into(),
        }
    }
    described
}

#[tokio::main]
async fn main() {
    let arguments = match read_arguments() {
        Ok(arguments) => arguments,
        Err(why) => {
            eprintln!("{why}");
            std::process::exit(2);
        }
    };
    let Ok(relay_url) = std::env::var("MOQ_TEST_RELAY_URL") else {
        eprintln!("MOQ_TEST_RELAY_URL is not set");
        std::process::exit(2);
    };
    let outcome = tokio::time::timeout(Duration::from_secs(arguments.timeout_seconds), async {
        let mut subscriber = connect(&relay_url, arguments.accept_any_certificate).await?;
        let namespace = moq_transport::coding::TrackNamespace::try_from(arguments.namespace.as_str())
            .map_err(|e| format!("not a namespace: {e}"))?;
        let (writer, reader) = moq_transport::serve::Track::new(namespace, arguments.track.as_str()).produce();
        tokio::spawn(async move {
            if let Err(failure) = subscriber.subscribe(writer).await {
                eprintln!("the subscription ended: {failure}");
            }
        });
        let moq_transport::serve::TrackReaderMode::Subgroups(mut subgroups) =
            reader.mode().await.map_err(|e| format!("the track did not open: {e}"))?
        else {
            return Err("the track is not subgroups".to_string());
        };
        let (mut seen, mut decrypted, mut parsed) = (0usize, 0usize, 0usize);
        let mut groups = 0usize;
        while seen < arguments.count {
            let Some(mut group) = subgroups.next().await.map_err(|e| e.to_string())? else {
                break;
            };
            groups += 1;
            while seen < arguments.count {
                match group.read_next().await {
                    Ok(Some(object)) => {
                        let described = describe(&object, arguments.key.as_ref());
                        if described["decrypted"] == true {
                            decrypted += 1;
                        }
                        if described.get("bag_keys").is_some() || described["frame_pixel_description_bytes"] != 0 {
                            parsed += 1;
                        }
                        seen += 1;
                        println!("{}", described);
                    }
                    _ => break,
                }
            }
        }
        Ok::<_, String>(serde_json::json!({
            "summary": true, "objects": seen, "groups_seen": groups, "decrypted": decrypted, "parsed": parsed
        }))
    })
    .await;
    match outcome {
        Ok(Ok(summary)) => println!("{summary}"),
        Ok(Err(why)) => {
            eprintln!("{why}");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("timed out");
            std::process::exit(1);
        }
    }
}
