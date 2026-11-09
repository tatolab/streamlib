// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One HTTP/1.1 request to a project's remote over the runtime's own rustls, its body streamed out
//! and its response body streamed in: the one HTTP client both a history remote's smart HTTP and
//! a content remote's Git LFS transfers go through.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::ConfigVerifierExt;

const REMOTE_HTTP_USER_AGENT: &str = "git/2.0 (tatolab runtime)";
const REMOTE_HTTP_CONNECT_AND_READ_TIMEOUT: Duration = Duration::from_secs(60);
const REMOTE_HTTP_COPY_CHUNK_BYTE_LEN: usize = 1 << 16;

/// One request to a remote: its method, URL and headers, and the length of the body sent.
pub(crate) struct HttpRequestToARemote<'request> {
    /// The HTTP method.
    pub method: &'request str,
    /// The whole URL, `https://` or `http://`, carrying no credentials.
    pub url: &'request str,
    /// Each header beside `Host`, `User-Agent`, `Connection` and `Content-Length`.
    pub headers: Vec<(String, String)>,
    /// The body's length; `None` sends no body and no `Content-Length`.
    pub body_byte_len: Option<u64>,
}

/// Send `request` with `request_body` and copy a 2xx response's body into `response_body`; any
/// other status refuses in the remote's words.
pub(crate) fn send_one_http_request_to_a_remote(
    request: &HttpRequestToARemote<'_>,
    request_body: &mut dyn Read,
    response_body: &mut dyn Write,
) -> std::io::Result<()> {
    let url_parts = remote_url_parts(request.url)?;
    let tcp = TcpStream::connect((url_parts.host.as_str(), url_parts.port))?;
    tcp.set_read_timeout(Some(REMOTE_HTTP_CONNECT_AND_READ_TIMEOUT))?;
    tcp.set_write_timeout(Some(REMOTE_HTTP_CONNECT_AND_READ_TIMEOUT))?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {REMOTE_HTTP_USER_AGENT}\r\nConnection: close\r\n",
        method = request.method,
        path = url_parts.path_and_query,
        host = url_parts.host_header,
    );
    for (name, value) in &request.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body_byte_len) = request.body_byte_len {
        head.push_str(&format!("Content-Length: {body_byte_len}\r\n"));
    }
    head.push_str("\r\n");

    if url_parts.uses_tls {
        let server_name = ServerName::try_from(url_parts.host.clone())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let connection =
            rustls::ClientConnection::new(the_runtimes_tls_client_config()?, server_name)
                .map_err(std::io::Error::other)?;
        let mut tls = rustls::StreamOwned::new(connection, tcp);
        exchange_one_request(&mut tls, &head, request, request_body, response_body)
    } else {
        let mut plain = tcp;
        exchange_one_request(&mut plain, &head, request, request_body, response_body)
    }
}

/// The scheme, host and port of `url`, as `https://host:port`, which decides whether two URLs
/// may be sent the same token.
pub(crate) fn origin_of_a_remote_url(url: &str) -> Option<String> {
    let url_parts = remote_url_parts(url).ok()?;
    Some(format!(
        "{}://{}:{}",
        if url_parts.uses_tls { "https" } else { "http" },
        url_parts.host,
        url_parts.port
    ))
}

struct RemoteUrlParts {
    uses_tls: bool,
    host: String,
    host_header: String,
    port: u16,
    path_and_query: String,
}

fn remote_url_parts(url: &str) -> std::io::Result<RemoteUrlParts> {
    let refused = |why: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the remote `{url}` {why}"),
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
        host_header: authority.to_owned(),
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

fn exchange_one_request(
    connection: &mut (impl Read + Write),
    head: &str,
    request: &HttpRequestToARemote<'_>,
    request_body: &mut dyn Read,
    response_body: &mut dyn Write,
) -> std::io::Result<()> {
    connection.write_all(head.as_bytes())?;
    if let Some(body_byte_len) = request.body_byte_len {
        let copied = std::io::copy(&mut request_body.take(body_byte_len), connection)?;
        if copied != body_byte_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "the body sent to `{}` ended after {copied} of its {body_byte_len} bytes",
                    request.url
                ),
            ));
        }
    }
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
                    "the remote `{}` answered no HTTP status: `{}`",
                    request.url,
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
                content_length = value.parse::<u64>().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }
    let is_a_success = (200..300).contains(&status_code);
    let mut refusal_body = Vec::new();
    let body_sink: &mut dyn Write = if is_a_success {
        response_body
    } else {
        &mut refusal_body
    };
    if chunked {
        copy_a_chunked_body(&mut response, body_sink)?;
    } else if let Some(content_length) = content_length {
        let copied = std::io::copy(&mut (&mut response).take(content_length), body_sink)?;
        if copied != content_length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "the remote `{}` closed after {copied} of the {content_length} bytes it announced",
                    request.url
                ),
            ));
        }
    } else {
        copy_until_the_remote_closes(&mut response, body_sink)?;
    }
    if !is_a_success {
        let why = match status_code {
            401 | 403 => "refused the token",
            404 => "holds no such repository",
            _ => "refused the request",
        };
        return Err(std::io::Error::other(format!(
            "the remote `{}` {why} (HTTP {status_code}): {}",
            request.url,
            String::from_utf8_lossy(&refusal_body).trim()
        )));
    }
    Ok(())
}

fn copy_a_chunked_body(response: &mut impl BufRead, body: &mut dyn Write) -> std::io::Result<()> {
    loop {
        let mut chunk_size_line = String::new();
        response.read_line(&mut chunk_size_line)?;
        let chunk_size_hex = chunk_size_line.trim().split(';').next().unwrap_or_default();
        let chunk_size = u64::from_str_radix(chunk_size_hex, 16).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "a chunk size was not hex")
        })?;
        if chunk_size == 0 {
            return Ok(());
        }
        let copied = std::io::copy(&mut (&mut *response).take(chunk_size), body)?;
        if copied != chunk_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "a chunk ended early",
            ));
        }
        let mut chunk_end = String::new();
        response.read_line(&mut chunk_end)?;
    }
}

/// Copy to the end; a TLS peer that closes without `close_notify` ends the body all the same.
fn copy_until_the_remote_closes(
    response: &mut impl Read,
    body: &mut dyn Write,
) -> std::io::Result<()> {
    let mut chunk = vec![0u8; REMOTE_HTTP_COPY_CHUNK_BYTE_LEN];
    loop {
        match response.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => body.write_all(&chunk[..read])?,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

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
        assert_eq!(local.host_header, "127.0.0.1:8123");
        let refused = remote_url_parts("https://x:secret@host/arm.git")
            .err()
            .unwrap();
        assert!(refused.to_string().contains("carries credentials"));
        assert!(remote_url_parts("ssh://host/arm.git").is_err());
    }

    #[test]
    fn a_chunked_body_is_joined() {
        let mut chunked = Cursor::new(b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\n\r\n".to_vec());
        let mut body = Vec::new();
        copy_a_chunked_body(&mut chunked, &mut body).unwrap();
        assert_eq!(body, b"Wikipedia");
    }

    #[test]
    fn two_urls_share_an_origin_only_on_the_same_scheme_host_and_port() {
        assert_eq!(
            origin_of_a_remote_url("https://host/a.git"),
            origin_of_a_remote_url("https://host:443/a.git/info/lfs")
        );
        assert_ne!(
            origin_of_a_remote_url("https://host/a.git"),
            origin_of_a_remote_url("https://bucket.host/a")
        );
        assert_ne!(
            origin_of_a_remote_url("https://host/a.git"),
            origin_of_a_remote_url("http://host/a.git")
        );
        assert_eq!(origin_of_a_remote_url("/a/local/path"), None);
    }
}
