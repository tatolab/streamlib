// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Git's smart HTTP protocol for libgit2, carried over the runtime's own rustls so libgit2 is built
//! without OpenSSL: each libgit2 action is one HTTP/1.1 request, a GET of `info/refs` for a ref
//! advertisement or a POST of the buffered pack negotiation to `git-upload-pack` or
//! `git-receive-pack`.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use git2::{Error as GitError, Remote};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::ConfigVerifierExt;

const SMART_HTTP_USER_AGENT: &str = "git/2.0 (tatolab runtime)";
const SMART_HTTP_CONNECT_AND_READ_TIMEOUT: Duration = Duration::from_secs(60);

thread_local! {
    /// The bearer token of the push or fetch running on this thread, which libgit2 drives synchronously.
    static BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `transfer` with `bearer_token` sent on every request it makes on this thread.
pub(crate) fn with_the_bearer_token_on_this_thread<TransferOutcome>(
    bearer_token: Option<&str>,
    transfer: impl FnOnce() -> TransferOutcome,
) -> TransferOutcome {
    register_the_smart_http_transport_once();
    BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD
        .with(|token| *token.borrow_mut() = bearer_token.map(str::to_owned));
    let outcome = transfer();
    BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD.with(|token| *token.borrow_mut() = None);
    outcome
}

fn register_the_smart_http_transport_once() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        for scheme in ["https", "http"] {
            // SAFETY: libgit2's transport registry is process-global and not thread-safe to
            // mutate; `Once` makes this the only registration, before any transfer runs.
            let registered = unsafe {
                git2::transport::register(scheme, |remote: &Remote<'_>| {
                    Transport::smart(remote, true, ProjectHistorySmartHttpSubtransport)
                })
            };
            if let Err(not_registered) = registered {
                tracing::error!(scheme, %not_registered, "the smart HTTP transport was not registered");
            }
        }
    });
}

struct ProjectHistorySmartHttpSubtransport;

impl SmartSubtransport for ProjectHistorySmartHttpSubtransport {
    fn action(
        &self,
        url: &str,
        action: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, GitError> {
        let base_url = url.trim_end_matches('/');
        let request = match action {
            Service::UploadPackLs => {
                SmartHttpRequestToTheRemote::advertisement(base_url, "git-upload-pack")
            }
            Service::ReceivePackLs => {
                SmartHttpRequestToTheRemote::advertisement(base_url, "git-receive-pack")
            }
            Service::UploadPack => {
                SmartHttpRequestToTheRemote::negotiation(base_url, "git-upload-pack")
            }
            Service::ReceivePack => {
                SmartHttpRequestToTheRemote::negotiation(base_url, "git-receive-pack")
            }
        };
        Ok(Box::new(SmartHttpExchangeWithTheRemote {
            request,
            request_body: Vec::new(),
            response_body: None,
            bearer_token: BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD
                .with(|token| token.borrow().clone()),
        }))
    }

    fn close(&self) -> Result<(), GitError> {
        Ok(())
    }
}

struct SmartHttpRequestToTheRemote {
    method: &'static str,
    url: String,
    content_type: Option<String>,
    accept: String,
}

impl SmartHttpRequestToTheRemote {
    fn advertisement(base_url: &str, service: &str) -> Self {
        Self {
            method: "GET",
            url: format!("{base_url}/info/refs?service={service}"),
            content_type: None,
            accept: format!("application/x-{service}-advertisement"),
        }
    }

    fn negotiation(base_url: &str, service: &str) -> Self {
        Self {
            method: "POST",
            url: format!("{base_url}/{service}"),
            content_type: Some(format!("application/x-{service}-request")),
            accept: format!("application/x-{service}-result"),
        }
    }
}

/// One request's buffered body going out, and its response's body once it is read.
struct SmartHttpExchangeWithTheRemote {
    request: SmartHttpRequestToTheRemote,
    request_body: Vec<u8>,
    response_body: Option<Cursor<Vec<u8>>>,
    bearer_token: Option<String>,
}

impl Write for SmartHttpExchangeWithTheRemote {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.request_body.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for SmartHttpExchangeWithTheRemote {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        if self.response_body.is_none() {
            let body = send_to_the_remote(
                &self.request,
                &self.request_body,
                self.bearer_token.as_deref(),
            )?;
            self.response_body = Some(Cursor::new(body));
        }
        self.response_body
            .as_mut()
            .map_or(Ok(0), |body| body.read(into))
    }
}

struct RemoteUrlParts {
    uses_tls: bool,
    host: String,
    port: u16,
    path_and_query: String,
}

fn remote_url_parts(url: &str) -> std::io::Result<RemoteUrlParts> {
    let refused = |why: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the history remote `{url}` {why}"),
        )
    };
    let (uses_tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(refused("is neither an https:// nor an http:// URL"));
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if authority.contains('@') {
        return Err(refused("carries credentials; pass the token on its own"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse()
                .map_err(|_| refused("names a port that is not a number"))?,
        ),
        None => (authority, if uses_tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(refused("names no host"));
    }
    Ok(RemoteUrlParts {
        uses_tls,
        host: host.to_owned(),
        port,
        path_and_query: if path.is_empty() {
            "/".to_owned()
        } else {
            path.to_owned()
        },
    })
}

fn the_runtimes_tls_client_config() -> std::io::Result<Arc<ClientConfig>> {
    static TLS_CLIENT_CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    TLS_CLIENT_CONFIG
        .get_or_init(|| {
            ClientConfig::with_platform_verifier()
                .map(Arc::new)
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|why| std::io::Error::other(format!("the TLS client could not be made: {why}")))
}

fn send_to_the_remote(
    request: &SmartHttpRequestToTheRemote,
    request_body: &[u8],
    bearer_token: Option<&str>,
) -> std::io::Result<Vec<u8>> {
    let url_parts = remote_url_parts(&request.url)?;
    let tcp = TcpStream::connect((url_parts.host.as_str(), url_parts.port))?;
    tcp.set_read_timeout(Some(SMART_HTTP_CONNECT_AND_READ_TIMEOUT))?;
    tcp.set_write_timeout(Some(SMART_HTTP_CONNECT_AND_READ_TIMEOUT))?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {SMART_HTTP_USER_AGENT}\r\nAccept: {accept}\r\nConnection: close\r\n",
        method = request.method,
        path = url_parts.path_and_query,
        host = url_parts.host,
        accept = request.accept,
    );
    if let Some(content_type) = &request.content_type {
        head.push_str(&format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
            request_body.len()
        ));
    }
    if let Some(bearer_token) = bearer_token {
        head.push_str(&format!("Authorization: Bearer {bearer_token}\r\n"));
    }
    head.push_str("\r\n");

    if url_parts.uses_tls {
        let server_name = ServerName::try_from(url_parts.host.clone())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let connection =
            rustls::ClientConnection::new(the_runtimes_tls_client_config()?, server_name)
                .map_err(std::io::Error::other)?;
        let mut tls = rustls::StreamOwned::new(connection, tcp);
        exchange_one_request(&mut tls, &head, request_body, &request.url)
    } else {
        let mut plain = tcp;
        exchange_one_request(&mut plain, &head, request_body, &request.url)
    }
}

fn exchange_one_request(
    connection: &mut (impl Read + Write),
    head: &str,
    request_body: &[u8],
    url: &str,
) -> std::io::Result<Vec<u8>> {
    connection.write_all(head.as_bytes())?;
    connection.write_all(request_body)?;
    connection.flush()?;

    let mut response = BufReader::new(connection);
    let mut status_line = String::new();
    response.read_line(&mut status_line)?;
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "the history remote `{url}` answered no HTTP status: `{}`",
                    status_line.trim()
                ),
            )
        })?;
    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut header_line = String::new();
        if response.read_line(&mut header_line)? == 0 || header_line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header_line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse::<usize>().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }
    let body = if chunked {
        read_a_chunked_body(&mut response)?
    } else if let Some(content_length) = content_length {
        let mut body = vec![0; content_length];
        response.read_exact(&mut body)?;
        body
    } else {
        read_until_the_remote_closes(&mut response)?
    };
    if !(200..300).contains(&status_code) {
        let why = match status_code {
            401 | 403 => "refused the token",
            404 => "holds no such repository",
            _ => "refused the request",
        };
        return Err(std::io::Error::other(format!(
            "the history remote `{url}` {why} (HTTP {status_code}): {}",
            String::from_utf8_lossy(&body).trim()
        )));
    }
    Ok(body)
}

fn read_a_chunked_body(response: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut chunk_size_line = String::new();
        response.read_line(&mut chunk_size_line)?;
        let chunk_size_hex = chunk_size_line.trim().split(';').next().unwrap_or_default();
        let chunk_size = usize::from_str_radix(chunk_size_hex, 16).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "a chunk size was not hex")
        })?;
        if chunk_size == 0 {
            return Ok(body);
        }
        let start = body.len();
        body.resize(start + chunk_size, 0);
        response.read_exact(&mut body[start..])?;
        let mut chunk_end = String::new();
        response.read_line(&mut chunk_end)?;
    }
}

/// Read to the end; a TLS peer that closes without `close_notify` ends the body all the same.
fn read_until_the_remote_closes(response: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    match response.read_to_end(&mut body) {
        Ok(_) => Ok(body),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(body),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_url_is_split_and_one_carrying_credentials_is_refused() {
        let parts = remote_url_parts(
            "https://abc.artifacts.cloudflare.net/git/team/arm.git/info/refs?service=git-upload-pack",
        )
        .unwrap();
        assert!(parts.uses_tls);
        assert_eq!(parts.host, "abc.artifacts.cloudflare.net");
        assert_eq!(parts.port, 443);
        assert_eq!(
            parts.path_and_query,
            "/git/team/arm.git/info/refs?service=git-upload-pack"
        );
        let local = remote_url_parts("http://127.0.0.1:8123/arm.git").unwrap();
        assert_eq!((local.uses_tls, local.port), (false, 8123));
        let refused = remote_url_parts("https://x:secret@host/arm.git")
            .err()
            .unwrap();
        assert!(refused.to_string().contains("carries credentials"));
        assert!(remote_url_parts("ssh://host/arm.git").is_err());
    }

    #[test]
    fn a_chunked_body_is_joined() {
        let mut chunked = Cursor::new(b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\n\r\n".to_vec());
        assert_eq!(read_a_chunked_body(&mut chunked).unwrap(), b"Wikipedia");
    }
}
