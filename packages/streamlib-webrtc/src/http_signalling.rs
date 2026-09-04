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
#[derive(Debug)]
pub(crate) struct OpenedSession {
    pub sdp_answer: String,
    /// Absolute, resolved from the `Location` header the relay returned.
    pub session_url: String,
}

/// Speaks WHIP or WHEP signalling to one endpoint.
pub(crate) struct WhipWhepSignallingClient {
    http_client: SignallingHttpClient,
    endpoint_url: String,
    bearer_token: Option<String>,
    protocol: &'static str,
}

impl WhipWhepSignallingClient {
    /// `protocol` names this client in every refusal it raises.
    pub(crate) fn new(
        endpoint_url: String,
        bearer_token: Option<String>,
        protocol: &'static str,
    ) -> Result<Self> {
        // Building the HTTPS connector reads rustls's default crypto provider
        // and *panics* inside rustls if nothing installed one. `load(host)`
        // installs it before any processor module is imported, so this is
        // already done in every real process — but a panic crossing into
        // Python is a worse failure than the idempotent call that prevents it.
        crate::transport_stack::bring_up()?;
        refuse_a_bearer_token_over_plaintext(&endpoint_url, bearer_token.as_deref(), protocol)?;

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
    pub(crate) async fn post_offer(&self, sdp_offer: &str) -> Result<OpenedSession> {
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
                let redirected_to = location_header(&headers, &endpoint_url).ok_or_else(|| {
                    self.refusal(format!("a {status} redirect carried no Location header"))
                })?;
                // A redirect this follows re-sends the Authorization header, so
                // a relay that answers with someone else's origin would be
                // handed the token. Refused rather than followed with the
                // header stripped: an ingest that redirects off-origin is not
                // a session this can open anyway.
                if self.bearer_token.is_some()
                    && origin_of(&redirected_to) != origin_of(&endpoint_url)
                {
                    return Err(self.refusal(format!(
                        "the relay redirected to a different origin ({}), and this session \
                         carries a bearer token that a redirect would disclose",
                        origin_of(&redirected_to).unwrap_or_default()
                    )));
                }
                refuse_a_bearer_token_over_plaintext(
                    &redirected_to,
                    self.bearer_token.as_deref(),
                    self.protocol,
                )?;
                endpoint_url = redirected_to;
                continue;
            }

            if status != StatusCode::CREATED {
                return Err(self.refusal(format!(
                    "the relay answered {status}: {}",
                    String::from_utf8_lossy(&body)
                )));
            }

            let session_url = location_header(&headers, &endpoint_url).ok_or_else(|| {
                self.refusal(
                    "a 201 Created carried no Location header, so the session \
                              cannot be trickled to or deleted"
                        .to_owned(),
                )
            })?;
            let sdp_answer = String::from_utf8(body.to_vec())
                .map_err(|failure| self.refusal(format!("the answer is not UTF-8: {failure}")))?;
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
    pub(crate) async fn patch_ice_candidates(
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
        if status.is_success() {
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
    pub(crate) async fn delete_session(&self, session_url: &str) {
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

    async fn collect_body(
        &self,
        response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<Bytes> {
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

/// Resolve a `Location` against the URL it was returned from.
///
/// RFC 9110 §10.2.2 allows any URI reference, and relays use most of them: an
/// absolute URL, a scheme-relative `//host/path`, a root-relative `/path`, or a
/// path-relative `sessions/7`. Reading a relative one as absolute would send
/// the next request nowhere.
fn location_header(headers: &hyper::HeaderMap, requested_url: &str) -> Option<String> {
    resolve_location(headers.get(header::LOCATION)?.to_str().ok()?, requested_url)
}

fn resolve_location(location: &str, requested_url: &str) -> Option<String> {
    if let Some(scheme_relative) = location.strip_prefix("//") {
        let scheme = requested_url.split_once("://")?.0;
        return Some(format!("{scheme}://{scheme_relative}"));
    }
    if is_absolute_url(location) {
        return Some(location.to_owned());
    }

    let origin = origin_of(requested_url)?;
    if let Some(root_relative) = location.strip_prefix('/') {
        return Some(format!("{origin}/{root_relative}"));
    }

    // Path-relative: against the requested path's directory, per RFC 3986 §5.3.
    let requested_path = requested_url.strip_prefix(&origin).unwrap_or("/");
    let directory = match requested_path.rfind('/') {
        Some(last_separator) => &requested_path[..=last_separator],
        None => "/",
    };
    Some(format!("{origin}{directory}{location}"))
}

/// A scheme followed by `://`, with nothing path-like before it.
fn is_absolute_url(url: &str) -> bool {
    url.split_once("://")
        .is_some_and(|(scheme, _)| !scheme.is_empty() && !scheme.contains(['/', '?', '#']))
}

/// Scheme and authority, which is what decides whether two URLs are one origin.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(format!("{scheme}://{authority}"))
}

/// A bearer token belongs only on a channel that hides it.
fn refuse_a_bearer_token_over_plaintext(
    url: &str,
    bearer_token: Option<&str>,
    protocol: &'static str,
) -> Result<()> {
    if bearer_token.is_some() && !hides_a_credential(url) {
        return Err(WebRtcExtensionError::Signalling {
            protocol,
            what: format!(
                "this session carries a bearer token and `{url}` is neither https nor \
                 loopback, so the token would cross the network in the clear"
            ),
        });
    }
    Ok(())
}

/// TLS, or a destination the bytes never leave the machine to reach — the same
/// carve-out a browser makes in treating `http://localhost` as a secure
/// context, and what lets a local relay be driven without a certificate.
fn hides_a_credential(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(authority) = url.strip_prefix("http://") else {
        return false;
    };
    let host = authority
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit_once(':')
        .map_or(
            authority.split(['/', '?', '#']).next().unwrap_or_default(),
            |(host, _port)| host,
        );
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") || host.starts_with("127.")
}

fn full_body(body: String) -> SignallingRequestBody {
    Full::new(Bytes::from(body))
        .map_err(|never| match never {})
        .boxed()
}

fn empty_body() -> SignallingRequestBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
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
    fn a_scheme_relative_location_keeps_the_requests_own_scheme() {
        assert_eq!(
            resolve_location(
                "//relay.example/sessions/7",
                "https://ingest.example/live/abc"
            )
            .as_deref(),
            Some("https://relay.example/sessions/7")
        );
    }

    #[test]
    fn a_path_relative_location_resolves_against_the_requests_directory() {
        assert_eq!(
            resolve_location("sessions/7", "https://ingest.example/live/abc").as_deref(),
            Some("https://ingest.example/live/sessions/7")
        );
    }

    #[test]
    fn a_bearer_token_is_refused_over_plaintext_but_allowed_to_loopback() {
        // Loopback never leaves the machine, which is the same carve-out a
        // browser makes for `http://localhost`.
        assert!(
            refuse_a_bearer_token_over_plaintext(
                "http://relay.example/live",
                Some("a-token"),
                "WHIP"
            )
            .is_err()
        );
        for loopback in [
            "http://127.0.0.1:8080/live",
            "http://localhost/live",
            "http://[::1]:9000/live",
        ] {
            assert!(
                refuse_a_bearer_token_over_plaintext(loopback, Some("a-token"), "WHIP").is_ok(),
                "{loopback} was refused"
            );
        }
        // No token, nothing to disclose.
        assert!(
            refuse_a_bearer_token_over_plaintext("http://relay.example/live", None, "WHIP").is_ok()
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

#[cfg(test)]
mod signalling_round_trips {
    use super::*;
    use crate::http_test_responder::HttpResponderUnderTest;

    const AN_ANSWER: &str = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n";

    fn created_at(location: &str) -> String {
        format!(
            "HTTP/1.1 201 Created\r\nLocation: {location}\r\nContent-Type: application/sdp\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{AN_ANSWER}",
            AN_ANSWER.len()
        )
    }

    fn redirected_to(location: &str) -> String {
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn client_for(endpoint_url: String, bearer_token: Option<String>) -> WhipWhepSignallingClient {
        WhipWhepSignallingClient::new(endpoint_url, bearer_token, "WHIP").unwrap()
    }

    #[tokio::test]
    async fn an_offer_is_posted_as_sdp_with_the_bearer_token_and_the_answer_read_back() {
        let relay = HttpResponderUnderTest::answering(vec![created_at("/sessions/7")]).await;
        let client = client_for(format!("{}/live", relay.origin), Some("a-token".to_owned()));

        let opened = client.post_offer("v=0\r\nthe-offer\r\n").await.unwrap();

        assert_eq!(opened.sdp_answer, AN_ANSWER);
        assert_eq!(opened.session_url, format!("{}/sessions/7", relay.origin));

        let recorded = relay.recorded();
        assert!(recorded[0].request_line.starts_with("POST /live "));
        assert!(recorded[0].has_header("content-type: application/sdp"));
        assert!(recorded[0].has_header("authorization: bearer a-token"));
        assert_eq!(recorded[0].body, "v=0\r\nthe-offer\r\n");
    }

    #[tokio::test]
    async fn no_token_configured_sends_no_authorization_header() {
        let relay = HttpResponderUnderTest::answering(vec![created_at("/sessions/7")]).await;
        let client = client_for(format!("{}/live", relay.origin), None);

        client.post_offer("v=0\r\n").await.unwrap();

        assert!(!relay.recorded()[0].has_header("authorization"));
    }

    #[tokio::test]
    async fn a_redirect_is_followed_and_the_offer_posted_again() {
        // The Location is root-relative, which is the form that has to be
        // resolved against the origin before it can be requested at all.
        let relay = HttpResponderUnderTest::answering(vec![
            redirected_to("/elsewhere"),
            created_at("/sessions/9"),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let opened = client.post_offer("v=0\r\nthe-offer\r\n").await.unwrap();

        assert_eq!(opened.session_url, format!("{}/sessions/9", relay.origin));
        let recorded = relay.recorded();
        assert!(recorded[0].request_line.starts_with("POST /live "));
        assert!(recorded[1].request_line.starts_with("POST /elsewhere "));
        assert_eq!(recorded[1].body, "v=0\r\nthe-offer\r\n");
    }

    #[tokio::test]
    async fn a_cross_origin_redirect_is_refused_rather_than_handed_the_token() {
        // Following it would re-send the Authorization header to whoever the
        // relay named. The offer is not worth disclosing the token for.
        let relay = HttpResponderUnderTest::answering(vec![redirected_to(
            "https://somewhere-else.example/sessions/9",
        )])
        .await;
        let client = client_for(format!("{}/live", relay.origin), Some("a-token".to_owned()));

        let refusal = client.post_offer("v=0\r\n").await.unwrap_err().to_string();

        assert!(refusal.contains("different origin"), "{refusal}");
        assert!(refusal.contains("bearer token"), "{refusal}");
    }

    #[tokio::test]
    async fn a_cross_origin_redirect_is_followed_when_there_is_no_token_to_disclose() {
        let elsewhere = HttpResponderUnderTest::answering(vec![created_at("/sessions/9")]).await;
        let relay = HttpResponderUnderTest::answering(vec![redirected_to(&format!(
            "{}/elsewhere",
            elsewhere.origin
        ))])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let opened = client.post_offer("v=0\r\n").await.unwrap();

        assert_eq!(
            opened.session_url,
            format!("{}/sessions/9", elsewhere.origin)
        );
    }

    #[tokio::test]
    async fn a_relay_that_redirects_forever_is_refused_rather_than_followed_forever() {
        let always_redirecting: Vec<String> = (0..HIGHEST_REDIRECTS_FOLLOWED + 3)
            .map(|_| redirected_to("/again"))
            .collect();
        let relay = HttpResponderUnderTest::answering(always_redirecting).await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let refusal = client.post_offer("v=0\r\n").await.unwrap_err();

        assert!(refusal.to_string().contains("redirected more than"));
    }

    #[tokio::test]
    async fn a_created_without_a_location_is_refused_because_the_session_is_unreachable() {
        let relay = HttpResponderUnderTest::answering(vec![
            "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let refusal = client.post_offer("v=0\r\n").await.unwrap_err();

        assert!(refusal.to_string().contains("Location"));
    }

    #[tokio::test]
    async fn a_refusal_from_the_relay_names_its_status_and_carries_its_body() {
        let relay = HttpResponderUnderTest::answering(vec![
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\n\
             bad-token   "
                .to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let refusal = client.post_offer("v=0\r\n").await.unwrap_err().to_string();

        assert!(refusal.contains("401"), "{refusal}");
        assert!(refusal.contains("bad-token"), "{refusal}");
    }

    #[tokio::test]
    async fn trickled_candidates_are_patched_to_the_session_url_as_an_sdp_fragment() {
        let relay = HttpResponderUnderTest::answering(vec![
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);
        let session_url = format!("{}/sessions/7", relay.origin);

        client
            .patch_ice_candidates(&session_url, "a=candidate:1 1 udp".to_owned())
            .await
            .unwrap();

        let recorded = relay.recorded();
        assert!(recorded[0].request_line.starts_with("PATCH /sessions/7 "));
        assert!(recorded[0].has_header("content-type: application/trickle-ice-sdpfrag"));
        assert_eq!(recorded[0].body, "a=candidate:1 1 udp");
    }

    #[tokio::test]
    async fn deleting_a_session_a_relay_refuses_is_reported_rather_than_raised() {
        let relay = HttpResponderUnderTest::answering(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        // Teardown is going away either way; a raise here would replace one
        // report with a worse one.
        client
            .delete_session(&format!("{}/sessions/7", relay.origin))
            .await;

        assert!(
            relay.recorded()[0]
                .request_line
                .starts_with("DELETE /sessions/7 ")
        );
    }
}
