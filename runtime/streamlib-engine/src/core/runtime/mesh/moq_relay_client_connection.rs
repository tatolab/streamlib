// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dialling a MoQ relay: one WebTransport session over QUIC, TLS 1.3 with the
//! platform's roots, `h3` as the ALPN and `moqt-16` as the WebTransport
//! subprotocol — the shape the MoQ extension wheel dials with, carried here so
//! the engine links no wheel.
//!
//! The relay's token rides the dial URL's path, so no error from here quotes
//! the URL; only its host.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::core::runtime::mesh::moq_gateway_configuration::the_relay_host_of;

/// The relay idles a connection out at roughly 10–15 s; QUIC speaks well
/// inside that.
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(4);

/// Workers the MoQ transport runtime runs its session, forwarder and drain
/// tasks on. Tasks, not threads per track.
const MOQ_TRANSPORT_RUNTIME_WORKER_THREADS: usize = 4;

/// The build's result, stored because `get_or_init` cannot fail.
static MOQ_TRANSPORT_RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

/// The runtime every MoQ session in this process runs on, with the rustls
/// provider installed first. Never dropped: a tokio runtime dropped inside an
/// async context panics.
pub(crate) fn the_moq_transport_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    if rustls::crypto::CryptoProvider::get_default().is_none()
        && rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
    {
        tracing::debug!("another caller installed the rustls crypto provider first");
    }
    MOQ_TRANSPORT_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(MOQ_TRANSPORT_RUNTIME_WORKER_THREADS)
                .thread_name("streamlib-moq-transport")
                .enable_all()
                .build()
                .map_err(|cannot_build| cannot_build.to_string())
        })
        .as_ref()
        .map_err(|cannot_build| format!("the MoQ transport runtime did not start: {cannot_build}"))
}

/// One MoQ session to a relay, both roles.
pub(crate) struct MoqRelaySession {
    pub(crate) session: moq_transport::session::Session,
    pub(crate) publisher: moq_transport::session::Publisher,
    pub(crate) subscriber: moq_transport::session::Subscriber,
}

/// Dial `relay_publish_url` and open a MoQ session on it.
pub(crate) async fn open_a_moq_session_to_the_relay(
    relay_publish_url: &str,
    accept_any_relay_certificate: bool,
) -> Result<MoqRelaySession, String> {
    let relay_host = the_relay_host_of(relay_publish_url);
    let dial_url = url::Url::parse(relay_publish_url)
        .map_err(|_| format!("the relay URL for {relay_host} is not a URL"))?;
    if dial_url.scheme() != "https" {
        return Err(format!(
            "the relay URL for {relay_host} must be https, and names {}",
            dial_url.scheme()
        ));
    }
    let provider = web_transport::quinn::crypto::default_provider();
    let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|failure| format!("the TLS 1.3 client config could not be built: {failure}"))?;
    let mut crypto = if accept_any_relay_certificate {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptsAnyRelayCertificate(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in rustls_native_certs::load_native_certs().certs {
            let _ = roots.add(certificate);
        }
        if roots.is_empty() {
            return Err(
                "no system root certificates were found, so no relay can be verified".to_string(),
            );
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    crypto.alpn_protocols = vec![web_transport::quinn::ALPN.as_bytes().to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .map_err(|failure| format!("the QUIC client config could not be built: {failure}"))?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
    client_config.transport_config(Arc::new(transport));

    let endpoint = open_a_client_endpoint()?;
    let subprotocol = std::str::from_utf8(moq_transport::setup::ALPN)
        .map_err(|_| "moq-transport's subprotocol name is not UTF-8".to_string())?;
    let request =
        web_transport::quinn::proto::ConnectRequest::new(dial_url).with_protocol(subprotocol);
    let web_transport_session = web_transport::quinn::Client::new(endpoint, client_config)
        .connect(request)
        .await
        .map_err(|failure| {
            format!("{relay_host} did not accept the WebTransport session: {failure}")
        })?;
    let (session, publisher, subscriber) = moq_transport::session::Session::connect(
        web_transport_session.into(),
        None,
        moq_transport::session::Transport::WebTransport,
    )
    .await
    .map_err(|failure| format!("the MoQ session with {relay_host} did not open: {failure}"))?;
    Ok(MoqRelaySession {
        session,
        publisher,
        subscriber,
    })
}

/// The UDP socket a QUIC client dials from — IPv6 unspecified first, IPv4 if
/// the host has no IPv6.
fn open_a_client_endpoint() -> Result<quinn::Endpoint, String> {
    let unspecified_ipv6: std::net::SocketAddr = (std::net::Ipv6Addr::UNSPECIFIED, 0).into();
    let unspecified_ipv4: std::net::SocketAddr = (std::net::Ipv4Addr::UNSPECIFIED, 0).into();
    quinn::Endpoint::client(unspecified_ipv6)
        .or_else(|_| quinn::Endpoint::client(unspecified_ipv4))
        .map_err(|failure| format!("no QUIC endpoint could be opened: {failure}"))
}

/// Dev-only verifier for a local self-signed relay: every certificate passes,
/// signatures are still checked against the certificate presented.
#[derive(Debug)]
struct AcceptsAnyRelayCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptsAnyRelayCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
