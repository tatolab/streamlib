// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bearer-token authentication for the api-server's mutating routes.
//!
//! An endpoint that mutates the runtime graph is remote code execution by
//! design, so every mutating route is gated behind a shared secret the
//! client presents as `Authorization: Bearer <token>`. The token is
//! auto-generated on first `setup()` and persisted at `0600` under the
//! streamlib data dir, so it survives restarts without being re-issued.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use streamlib::sdk::error::{Error, Result};

/// Subdirectory (under the streamlib data dir) that holds the api-server's
/// persisted control-plane secrets.
const AUTH_TOKEN_SUBDIR: &str = "api-server";

/// Filename of the persisted bearer token within [`AUTH_TOKEN_SUBDIR`].
const AUTH_TOKEN_FILE: &str = "auth-token";

/// Number of OS-random bytes drawn per fresh token; hex-encoded to a
/// 64-character secret. 256 bits of entropy — brute-force-infeasible.
const TOKEN_RANDOM_BYTES: usize = 32;

/// The api-server's bearer token: the shared secret a client presents on
/// every mutating route. Cheap to clone (the secret sits behind an [`Arc`]),
/// so it doubles as the auth middleware's state.
#[derive(Clone)]
pub(crate) struct ApiServerBearerToken {
    secret: Arc<String>,
}

impl ApiServerBearerToken {
    /// Load the persisted token from `<data_dir>/api-server/auth-token`, or
    /// generate and persist a fresh one at `0600` if absent. Reused across
    /// restarts so a client's stored credential keeps working.
    pub(crate) fn load_or_create_under_data_dir() -> Result<Self> {
        Self::load_or_create_at(&Self::default_token_path())
    }

    /// The on-disk location of the persisted token under the streamlib data
    /// dir. Exposed so a caller can surface it to an operator provisioning a
    /// client credential.
    pub(crate) fn default_token_path() -> PathBuf {
        streamlib::sdk::home::get_streamlib_data_dir()
            .join(AUTH_TOKEN_SUBDIR)
            .join(AUTH_TOKEN_FILE)
    }

    /// Path-explicit form of [`Self::load_or_create_under_data_dir`], factored
    /// out so tests can drive token persistence against a tempdir without
    /// depending on the process-global `$STREAMLIB_HOME`.
    pub(crate) fn load_or_create_at(path: &Path) -> Result<Self> {
        if let Some(existing) = read_persisted_token(path)? {
            return Ok(Self {
                secret: Arc::new(existing),
            });
        }
        let secret = generate_token()?;
        persist_token_0600(path, &secret)?;
        Ok(Self {
            secret: Arc::new(secret),
        })
    }

    /// Construct from a known secret — test-only wiring for the middleware.
    #[cfg(test)]
    pub(crate) fn from_secret(secret: impl Into<String>) -> Self {
        Self {
            secret: Arc::new(secret.into()),
        }
    }

    /// Constant-time compare of a presented token against the secret:
    /// constant-time over the compared bytes (equal-length inputs leak no
    /// byte-position timing).
    fn matches(&self, presented: &str) -> bool {
        constant_time_eq::constant_time_eq(self.secret.as_bytes(), presented.as_bytes())
    }
}

/// Read an existing, non-empty token file. `Ok(None)` when the file is absent
/// (first run) or empty (a truncated write); any other IO error propagates.
fn read_persisted_token(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Draw [`TOKEN_RANDOM_BYTES`] from the OS CSPRNG and hex-encode them.
fn generate_token() -> Result<String> {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| {
        Error::Runtime(format!(
            "ApiServer: OS RNG unavailable while generating bearer token: {e}"
        ))
    })?;
    let mut token = String::with_capacity(TOKEN_RANDOM_BYTES * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

/// Write the token at `0600`, creating the parent dir as needed. The file is
/// opened with mode `0600` at creation and re-`chmod`'d afterward so a
/// pre-existing file with looser bits is tightened.
fn persist_token_0600(path: &Path, secret: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(secret.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Body for `401 Unauthorized` — no usable `Authorization: Bearer` header was
/// presented. Mirrors the typed-discriminator shape of the graph error
/// responses in [`crate::state`].
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct UnauthorizedResponse {
    /// Typed error discriminator: always `"MissingBearerToken"`.
    pub error: &'static str,
}

/// Body for `403 Forbidden` — a bearer token was presented but did not match
/// the server's secret.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ForbiddenResponse {
    /// Typed error discriminator: always `"InvalidBearerToken"`.
    pub error: &'static str,
}

/// Auth middleware gating the mutating routes: rejects a missing / malformed
/// `Authorization` header with `401`, a wrong token with `403`, and otherwise
/// runs the inner handler. Its state (the expected token) is supplied by
/// [`axum::middleware::from_fn_with_state`], independent of the router's
/// `AppState`.
pub(crate) async fn require_bearer_token(
    State(expected): State<ApiServerBearerToken>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token_from_header);

    match presented {
        None => (
            StatusCode::UNAUTHORIZED,
            Json(UnauthorizedResponse {
                error: "MissingBearerToken",
            }),
        )
            .into_response(),
        Some(token) if expected.matches(token) => next.run(request).await,
        Some(_) => (
            StatusCode::FORBIDDEN,
            Json(ForbiddenResponse {
                error: "InvalidBearerToken",
            }),
        )
            .into_response(),
    }
}

/// Extract the credential from an `Authorization` header value, accepting the
/// case-insensitive `Bearer` scheme (RFC 7235 §2.1). Returns `None` for any
/// other scheme or an empty credential.
fn bearer_token_from_header(header: &str) -> Option<&str> {
    let (scheme, credential) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let credential = credential.trim();
    if credential.is_empty() {
        None
    } else {
        Some(credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request, routing::post};
    use tower::ServiceExt;

    fn protected_test_router(token: ApiServerBearerToken) -> Router {
        Router::new()
            .route("/mutate", post(|| async { StatusCode::OK }))
            .route_layer(axum::middleware::from_fn_with_state(
                token,
                require_bearer_token,
            ))
    }

    async fn status_for_auth_header(auth: Option<&str>) -> StatusCode {
        let router = protected_test_router(ApiServerBearerToken::from_secret("correct-horse"));
        let mut builder = Request::builder().method("POST").uri("/mutate");
        if let Some(value) = auth {
            builder = builder.header(AUTHORIZATION, value);
        }
        let request = builder.body(Body::empty()).unwrap();
        router.oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn missing_token_is_401() {
        assert_eq!(status_for_auth_header(None).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn malformed_authorization_header_is_401() {
        // No scheme / wrong scheme → treated as no credential presented.
        assert_eq!(
            status_for_auth_header(Some("correct-horse")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_for_auth_header(Some("Basic correct-horse")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_for_auth_header(Some("Bearer ")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn wrong_token_is_403() {
        assert_eq!(
            status_for_auth_header(Some("Bearer wrong-token")).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn valid_token_passes_through() {
        assert_eq!(
            status_for_auth_header(Some("Bearer correct-horse")).await,
            StatusCode::OK
        );
        // Scheme match is case-insensitive per RFC 7235.
        assert_eq!(
            status_for_auth_header(Some("bearer correct-horse")).await,
            StatusCode::OK
        );
    }

    #[test]
    fn token_is_generated_persisted_0600_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api-server").join("auth-token");

        let first = ApiServerBearerToken::load_or_create_at(&path).unwrap();
        assert!(path.exists(), "token file must be persisted");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-only (0600)");

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents.trim().len(),
            TOKEN_RANDOM_BYTES * 2,
            "hex-encoded token is two chars per random byte"
        );

        // A second load reuses the persisted secret rather than re-issuing.
        let second = ApiServerBearerToken::load_or_create_at(&path).unwrap();
        assert!(second.matches(contents.trim()));
        assert!(first.matches(contents.trim()));
    }

    #[test]
    fn generated_tokens_are_distinct() {
        assert_ne!(generate_token().unwrap(), generate_token().unwrap());
    }
}
