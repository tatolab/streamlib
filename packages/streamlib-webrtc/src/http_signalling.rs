// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The HTTP half of WHIP (RFC 9725) and WHEP, which are the same three
//! requests: POST the offer, PATCH trickled candidates, DELETE the session.

use crate::error::{Result, WebRtcExtensionError};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{Request, StatusCode, header};
use std::time::Duration;

type SignallingRequestBody = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
type SignallingHttpClient = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    SignallingRequestBody,
>;

/// How long a signalling request may take. Bounded well under the helper's
/// 60 s registration budget, which a connect on the first bag sits outside but
/// a stalled relay would otherwise hold a processor in indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The DELETE that ends a session runs during teardown, which the engine bounds
/// at five seconds before it kills the child — so this has to finish first.
const TEARDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// A relay that answers every redirect with another one would otherwise spin
/// here for as long as it liked.
const HIGHEST_REDIRECTS_FOLLOWED: u8 = 5;

/// The session a successful offer opened.
pub struct OpenedSession {
    pub sdp_answer: String,
    /// Absolute, resolved from the `Location` header the relay returned.
    pub session_url: String,
}

/// Speaks WHIP or WHEP signalling to one endpoint.
pub struct WhipWhepSignallingClient {
    http_client: SignallingHttpClient,
    endpoint_url: String,
    bearer_token: Option<String>,
    protocol: &'static str,
}

impl WhipWhepSignallingClient {
    /// `protocol` names this client in every refusal it raises.
    pub fn new(
        endpoint_url: String,
        bearer_token: Option<String>,
        protocol: &'static str,
    ) -> Result<Self> {
        let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|failure| WebRtcExtensionError::Signalling {
                protocol,
                what: format!("no system CA roots to verify the relay against: {failure}"),
            })?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        Ok(Self {
            http_client: hyper_util::client::legacy::Client::builder(
                hyper_util::rt::TokioExecutor::new(),
            )
            .pool_idle_timeout(Duration::from_secs(30))
            .build(https_connector),
            endpoint_url,
            bearer_token,
            protocol,
        })
    }

    /// POST the offer and read back the answer and the session's own URL.
    pub async fn post_offer(&self, sdp_offer: &str) -> Result<OpenedSession> {
        let mut endpoint_url = self.endpoint_url.clone();

        for _ in 0..=HIGHEST_REDIRECTS_FOLLOWED {
            let request = self
                .request_builder("POST", &endpoint_url)
                .header(header::CONTENT_TYPE, "application/sdp")
                .body(full_body(sdp_offer.to_owned()))
                .map_err(|failure| self.refusal(format!("could not build the offer: {failure}")))?;

            let response = self.send(request, REQUEST_TIMEOUT).await?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = self.collect_body(response).await?;

            if status == StatusCode::TEMPORARY_REDIRECT || status == StatusCode::PERMANENT_REDIRECT
            {
                endpoint_url = location_header(&headers, &endpoint_url).ok_or_else(|| {
                    self.refusal(format!("a {status} redirect carried no Location header"))
                })?;
                continue;
            }

            if status != StatusCode::CREATED {
                return Err(self.refusal(format!(
                    "the relay answered {status}: {}",
                    String::from_utf8_lossy(&body)
                )));
            }

            let session_url = location_header(&headers, &endpoint_url).ok_or_else(|| {
                self.refusal("a 201 Created carried no Location header, so the session \
                              cannot be trickled to or deleted"
                    .to_owned())
            })?;
            let sdp_answer = String::from_utf8(body.to_vec()).map_err(|failure| {
                self.refusal(format!("the answer is not UTF-8: {failure}"))
            })?;
            return Ok(OpenedSession {
                sdp_answer,
                session_url,
            });
        }

        Err(self.refusal(format!(
            "the relay redirected more than {HIGHEST_REDIRECTS_FOLLOWED} times"
        )))
    }

    /// Trickle gathered candidates onto an open session.
    pub async fn patch_ice_candidates(
        &self,
        session_url: &str,
        sdp_fragment: String,
    ) -> Result<()> {
        let request = self
            .request_builder("PATCH", session_url)
            .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag")
            .body(full_body(sdp_fragment))
            .map_err(|failure| self.refusal(format!("could not build the PATCH: {failure}")))?;

        let response = self.send(request, REQUEST_TIMEOUT).await?;
        let status = response.status();
        if status == StatusCode::NO_CONTENT || status.is_success() {
            return Ok(());
        }
        let body = self.collect_body(response).await?;
        Err(self.refusal(format!(
            "trickling candidates was answered {status}: {}",
            String::from_utf8_lossy(&body)
        )))
    }

    /// End the session. A relay that refuses is logged rather than raised:
    /// this runs during teardown, where the local side is going away either
    /// way and a raise would only replace one report with a worse one.
    pub async fn delete_session(&self, session_url: &str) {
        let request = match self
            .request_builder("DELETE", session_url)
            .body(empty_body())
        {
            Ok(request) => request,
            Err(failure) => {
                tracing::warn!(protocol = self.protocol, %failure, "could not build the DELETE");
                return;
            }
        };

        match self.send(request, TEARDOWN_REQUEST_TIMEOUT).await {
            Ok(response) if response.status().is_success() => {
                tracing::info!(protocol = self.protocol, session_url, "session deleted");
            }
            Ok(response) => {
                tracing::warn!(
                    protocol = self.protocol,
                    status = %response.status(),
                    "the relay refused the session delete; it may linger server-side"
                );
            }
            Err(failure) => {
                tracing::warn!(protocol = self.protocol, %failure, "the session delete did not complete");
            }
        }
    }

    fn request_builder(&self, method: &str, url: &str) -> hyper::http::request::Builder {
        let builder = Request::builder().method(method).uri(url);
        match &self.bearer_token {
            Some(token) => builder.header(header::AUTHORIZATION, format!("Bearer {token}")),
            None => builder,
        }
    }

    async fn send(
        &self,
        request: Request<SignallingRequestBody>,
        timeout: Duration,
    ) -> Result<hyper::Response<hyper::body::Incoming>> {
        tokio::time::timeout(timeout, self.http_client.request(request))
            .await
            .map_err(|_| self.refusal(format!("the relay did not answer within {timeout:?}")))?
            .map_err(|failure| self.refusal(format!("the request did not complete: {failure}")))
    }

    async fn collect_body(&self, response: hyper::Response<hyper::body::Incoming>) -> Result<Bytes> {
        Ok(BodyExt::collect(response.into_body())
            .await
            .map_err(|failure| self.refusal(format!("could not read the response: {failure}")))?
            .to_bytes())
    }

    fn refusal(&self, what: String) -> WebRtcExtensionError {
        WebRtcExtensionError::Signalling {
            protocol: self.protocol,
            what,
        }
    }
}

/// Resolve a `Location` against the URL it was returned from, since a relay may
/// answer with an absolute URL or a site-root-relative path.
fn location_header(headers: &hyper::HeaderMap, requested_url: &str) -> Option<String> {
    let location = headers.get(header::LOCATION)?.to_str().ok()?;
    if !location.starts_with('/') {
        return Some(location.to_owned());
    }
    let origin: String = requested_url.split('/').take(3).collect::<Vec<_>>().join("/");
    Some(format!("{origin}{location}"))
}

fn full_body(body: String) -> SignallingRequestBody {
    Full::new(Bytes::from(body))
        .map_err(|never| match never {})
        .boxed()
}

fn empty_body() -> SignallingRequestBody {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_location(location: &str) -> hyper::HeaderMap {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(header::LOCATION, location.parse().unwrap());
        headers
    }

    #[test]
    fn an_absolute_location_is_taken_as_it_stands() {
        let resolved = location_header(
            &headers_with_location("https://relay.example/sessions/7"),
            "https://ingest.example/live/abc",
        );

        assert_eq!(
            resolved.as_deref(),
            Some("https://relay.example/sessions/7")
        );
    }

    #[test]
    fn a_root_relative_location_resolves_against_the_endpoints_origin() {
        let resolved = location_header(
            &headers_with_location("/sessions/7"),
            "https://ingest.example/live/abc",
        );

        assert_eq!(
            resolved.as_deref(),
            Some("https://ingest.example/sessions/7")
        );
    }

    #[test]
    fn a_response_without_a_location_resolves_to_nothing() {
        assert_eq!(
            location_header(&hyper::HeaderMap::new(), "https://ingest.example/live"),
            None
        );
    }
}
