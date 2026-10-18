// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The TOML file one `moq-relay` child is started with, in the 0.17 line's
//! keys: `listen.bind`, `listen.tls.cert` / `listen.tls.key`,
//! `web.http.listen`, `auth.url`, `drain_timeout`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Serialize;
use streamlib_relay_adapter_sdk::url::Url;

/// How long the child lets its sessions drain once told to stop; `tatolabd`
/// is ending when it says so.
const CHILD_DRAIN_TIMEOUT: &str = "1s";

/// One child's configuration: where it listens, the certificate it serves,
/// and the socket its admission questions go to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRelayChildConfiguration {
    /// Where QUIC and HTTP alike bind.
    pub bind_address: SocketAddr,
    /// The certificate chain the child serves, PEM.
    pub certificate_path: PathBuf,
    /// The certificate's private key, PEM.
    pub private_key_path: PathBuf,
    /// The unix socket `tatolabd` answers the child's admission requests on.
    pub admission_socket_path: PathBuf,
}

#[derive(Serialize)]
struct RelayConfigurationFile {
    listen: RelayListenTable,
    web: RelayWebTable,
    auth: RelayAuthTable,
    drain_timeout: &'static str,
}

#[derive(Serialize)]
struct RelayListenTable {
    bind: String,
    tls: RelayListenTlsTable,
}

#[derive(Serialize)]
struct RelayListenTlsTable {
    cert: String,
    key: String,
}

#[derive(Serialize)]
struct RelayWebTable {
    http: RelayWebHttpTable,
}

#[derive(Serialize)]
struct RelayWebHttpTable {
    listen: String,
}

#[derive(Serialize)]
struct RelayAuthTable {
    url: String,
}

impl LocalRelayChildConfiguration {
    /// The file's text.
    pub fn rendered_as_toml(&self) -> Result<String, LocalRelayChildConfigurationRefusal> {
        let file = RelayConfigurationFile {
            listen: RelayListenTable {
                bind: self.bind_address.to_string(),
                tls: RelayListenTlsTable {
                    cert: path_text(&self.certificate_path),
                    key: path_text(&self.private_key_path),
                },
            },
            web: RelayWebTable {
                http: RelayWebHttpTable {
                    listen: self.bind_address.to_string(),
                },
            },
            auth: RelayAuthTable {
                url: admission_url_of_the_socket(&self.admission_socket_path)?,
            },
            drain_timeout: CHILD_DRAIN_TIMEOUT,
        };
        Ok(toml::to_string(&file)?)
    }
}

/// The `unix://` URL `moq-relay` speaks HTTP to `socket_path` over; the relay
/// reads the socket back with `Url::to_file_path`, so the path is
/// percent-encoded as a URL path.
fn admission_url_of_the_socket(
    socket_path: &Path,
) -> Result<String, LocalRelayChildConfigurationRefusal> {
    let mut admission_url = Url::parse("unix:///")?;
    admission_url.set_path(&path_text(socket_path));
    Ok(admission_url.into())
}

/// Why a child's configuration could not be rendered.
#[derive(Debug, thiserror::Error)]
pub enum LocalRelayChildConfigurationRefusal {
    #[error("its admission socket's URL could not be formed: {0}")]
    AdmissionUrlNotFormed(#[from] streamlib_relay_adapter_sdk::url::ParseError),
    #[error("its configuration could not be rendered as TOML: {0}")]
    NotRendered(#[from] toml::ser::Error),
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_child_listens_serves_its_web_routes_and_asks_admission_where_it_is_told() {
        let configuration = LocalRelayChildConfiguration {
            bind_address: "127.0.0.1:4443".parse().unwrap(),
            certificate_path: PathBuf::from("/run/r/local-loopback/certificate.pem"),
            private_key_path: PathBuf::from("/run/r/local-loopback/private-key.pem"),
            admission_socket_path: PathBuf::from("/run/r/local-loopback/admission.sock"),
        };
        let rendered: toml::Value = configuration.rendered_as_toml().unwrap().parse().unwrap();
        assert_eq!(
            rendered,
            toml::toml! {
                drain_timeout = "1s"
                [listen]
                bind = "127.0.0.1:4443"
                [listen.tls]
                cert = "/run/r/local-loopback/certificate.pem"
                key = "/run/r/local-loopback/private-key.pem"
                [web.http]
                listen = "127.0.0.1:4443"
                [auth]
                url = "unix:///run/r/local-loopback/admission.sock"
            }
            .into()
        );
    }

    #[test]
    fn an_every_interface_child_binds_the_wildcard_for_quic_and_http_alike() {
        let configuration = LocalRelayChildConfiguration {
            bind_address: "[::]:4445".parse().unwrap(),
            certificate_path: PathBuf::from("/c.pem"),
            private_key_path: PathBuf::from("/k.pem"),
            admission_socket_path: PathBuf::from("/a.sock"),
        };
        let rendered: toml::Value = configuration.rendered_as_toml().unwrap().parse().unwrap();
        assert_eq!(rendered["listen"]["bind"].as_str(), Some("[::]:4445"));
        assert_eq!(
            rendered["web"]["http"]["listen"].as_str(),
            Some("[::]:4445")
        );
    }

    #[test]
    fn an_admission_socket_path_is_read_back_whole_from_its_url() {
        let socket_path = PathBuf::from("/tmp/tl dir/run/relays/local-loopback/admission.sock");
        let admission_url: Url = admission_url_of_the_socket(&socket_path)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(admission_url.scheme(), "unix");
        assert_eq!(admission_url.to_file_path(), Ok(socket_path));
    }
}
