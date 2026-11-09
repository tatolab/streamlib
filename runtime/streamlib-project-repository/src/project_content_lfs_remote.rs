// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project's content remote: the Git LFS endpoint its asset bytes are uploaded to and downloaded
//! from beside its history remote, spoken through the LFS batch API and its `basic` transfer.

use std::collections::BTreeMap;
use std::fs::File;

use serde::{Deserialize, Serialize};

use crate::project_content_store::{ProjectContentStore, StoredProjectContent};
use crate::project_history_repository::ProjectHistoryRemote;
use crate::project_remote_http_exchange::{
    HttpRequestToARemote, origin_of_a_remote_url, send_one_http_request_to_a_remote,
};
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The file of a project, tracked in its history, whose `[lfs] url` names its content remote, as
/// Git LFS reads it.
pub const PROJECT_LFS_CONFIG_FILE_NAME: &str = ".lfsconfig";

const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";
const LFS_OBJECTS_PER_BATCH_REQUEST: usize = 100;

/// A Git LFS endpoint a project's asset bytes go to and come from, named by SHA-256 and size.
#[derive(Clone)]
pub struct ProjectContentRemote {
    /// The LFS endpoint, `<endpoint>/objects/batch` being its batch API; it carries no credentials.
    pub lfs_endpoint_url: String,
    /// The token sent as `Authorization: Bearer` to the endpoint, and to a transfer at its own
    /// origin that names no `Authorization` of its own.
    pub bearer_token: Option<String>,
}

impl std::fmt::Debug for ProjectContentRemote {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectContentRemote")
            .field("lfs_endpoint_url", &self.lfs_endpoint_url)
            .field("bearer_token", &self.bearer_token.as_ref().map(|_| "set"))
            .finish()
    }
}

impl ProjectContentRemote {
    /// The content remote beside `history_remote`: its own when it names one, else the `[lfs] url`
    /// of `lfs_config`, the project's `.lfsconfig`, else Git LFS's default `<url>/info/lfs`. The
    /// history remote's token goes along only to an endpoint at the history remote's own origin.
    /// A history remote that is no `https://` or `http://` URL, a directory on this machine, has none.
    pub fn beside_the_history_remote(
        history_remote: &ProjectHistoryRemote,
        lfs_config: Option<&[u8]>,
    ) -> Option<Self> {
        if let Some(content_remote) = &history_remote.content_remote {
            return Some(content_remote.clone());
        }
        let history_origin = origin_of_a_remote_url(&history_remote.url)?;
        let lfs_endpoint_url = lfs_config
            .and_then(the_lfs_url_an_lfs_config_names)
            .unwrap_or_else(|| the_default_lfs_endpoint_of(&history_remote.url));
        let bearer_token = (origin_of_a_remote_url(&lfs_endpoint_url).as_ref()
            == Some(&history_origin))
        .then(|| history_remote.bearer_token.clone())
        .flatten();
        Some(Self {
            lfs_endpoint_url,
            bearer_token,
        })
    }

    /// Upload each of `contents`, which `content_store` holds, that the remote lacks, answering how
    /// many were sent.
    pub fn upload_each_content_it_lacks(
        &self,
        content_store: &ProjectContentStore,
        contents: &[StoredProjectContent],
    ) -> Result<usize, ProjectRepositoryRefusal> {
        let mut uploaded = 0;
        for batch in contents.chunks(LFS_OBJECTS_PER_BATCH_REQUEST) {
            for object in self.batch(LfsOperation::Upload, batch)? {
                let content = self.content_the_remote_answered_for(&object, batch)?;
                if let Some(error) = &object.error {
                    return Err(self.refused(format!(
                        "it refused to take {}: {} (LFS {})",
                        object.oid, error.message, error.code
                    )));
                }
                let Some(upload) = object.actions.get("upload") else {
                    continue;
                };
                let mut stored_file =
                    File::open(content_store.content_path(&content.content_sha256))?;
                let mut headers = self.headers_of_a_transfer(upload);
                headers.push((
                    "Content-Type".to_owned(),
                    "application/octet-stream".to_owned(),
                ));
                send_one_http_request_to_a_remote(
                    &HttpRequestToARemote {
                        method: "PUT",
                        url: &upload.href,
                        headers,
                        body_byte_len: Some(content.content_byte_len),
                    },
                    &mut stored_file,
                    &mut std::io::sink(),
                )
                .map_err(|not_sent| self.refused(not_sent.to_string()))?;
                if let Some(verify) = object.actions.get("verify") {
                    let verify_body = serde_json::to_vec(&LfsObjectNamed {
                        oid: content.content_sha256.clone(),
                        size: content.content_byte_len,
                    })?;
                    let mut headers = self.headers_of_a_transfer(verify);
                    headers.push(("Content-Type".to_owned(), LFS_MEDIA_TYPE.to_owned()));
                    headers.push(("Accept".to_owned(), LFS_MEDIA_TYPE.to_owned()));
                    send_one_http_request_to_a_remote(
                        &HttpRequestToARemote {
                            method: "POST",
                            url: &verify.href,
                            headers,
                            body_byte_len: Some(verify_body.len() as u64),
                        },
                        &mut verify_body.as_slice(),
                        &mut std::io::sink(),
                    )
                    .map_err(|not_verified| self.refused(not_verified.to_string()))?;
                }
                uploaded += 1;
            }
        }
        tracing::info!(
            remote = %self.lfs_endpoint_url,
            uploaded,
            offered = contents.len(),
            "uploaded the project's asset bytes the content remote lacked"
        );
        Ok(uploaded)
    }

    /// Download each of `contents` that `content_store` does not hold into it, each kept only when
    /// its bytes hash to its name.
    pub fn download_each_content_into(
        &self,
        content_store: &ProjectContentStore,
        contents: &[StoredProjectContent],
    ) -> Result<(), ProjectRepositoryRefusal> {
        let missing: Vec<StoredProjectContent> = contents
            .iter()
            .filter(|content| {
                content_store
                    .stored_content(&content.content_sha256)
                    .is_err()
            })
            .cloned()
            .collect();
        for batch in missing.chunks(LFS_OBJECTS_PER_BATCH_REQUEST) {
            for object in self.batch(LfsOperation::Download, batch)? {
                let content = self.content_the_remote_answered_for(&object, batch)?;
                if let Some(error) = &object.error {
                    return Err(ProjectRepositoryRefusal::ContentRemoteHoldsNoSuchContent {
                        url: self.lfs_endpoint_url.clone(),
                        content_sha256: content.content_sha256.clone(),
                        why: format!("{} (LFS {})", error.message, error.code),
                    });
                }
                let download = object.actions.get("download").ok_or_else(|| {
                    ProjectRepositoryRefusal::ContentRemoteHoldsNoSuchContent {
                        url: self.lfs_endpoint_url.clone(),
                        content_sha256: content.content_sha256.clone(),
                        why: "it answered no download".to_owned(),
                    }
                })?;
                content_store.store_content_expected_to_be(content, |partial_file| {
                    send_one_http_request_to_a_remote(
                        &HttpRequestToARemote {
                            method: "GET",
                            url: &download.href,
                            headers: self.headers_of_a_transfer(download),
                            body_byte_len: None,
                        },
                        &mut std::io::empty(),
                        partial_file,
                    )
                    .map_err(|not_received| self.refused(not_received.to_string()))
                })?;
            }
        }
        tracing::info!(
            remote = %self.lfs_endpoint_url,
            downloaded = missing.len(),
            "downloaded the asset bytes this project's content store lacked"
        );
        Ok(())
    }

    fn batch(
        &self,
        operation: LfsOperation,
        contents: &[StoredProjectContent],
    ) -> Result<Vec<LfsBatchResponseObject>, ProjectRepositoryRefusal> {
        let request_body = serde_json::to_vec(&LfsBatchRequest {
            operation,
            transfers: vec!["basic"],
            hash_algo: "sha256",
            objects: contents
                .iter()
                .map(|content| LfsObjectNamed {
                    oid: content.content_sha256.clone(),
                    size: content.content_byte_len,
                })
                .collect(),
        })?;
        let mut headers = vec![
            ("Accept".to_owned(), LFS_MEDIA_TYPE.to_owned()),
            ("Content-Type".to_owned(), LFS_MEDIA_TYPE.to_owned()),
        ];
        if let Some(bearer_token) = &self.bearer_token {
            headers.push(("Authorization".to_owned(), format!("Bearer {bearer_token}")));
        }
        let batch_url = format!(
            "{}/objects/batch",
            self.lfs_endpoint_url.trim_end_matches('/')
        );
        let mut response_body = Vec::new();
        send_one_http_request_to_a_remote(
            &HttpRequestToARemote {
                method: "POST",
                url: &batch_url,
                headers,
                body_byte_len: Some(request_body.len() as u64),
            },
            &mut request_body.as_slice(),
            &mut response_body,
        )
        .map_err(|not_answered| self.refused(not_answered.to_string()))?;
        let response: LfsBatchResponse = serde_json::from_slice(&response_body)
            .map_err(|e| self.refused(format!("its batch answer is no LFS batch response: {e}")))?;
        if response
            .transfer
            .as_deref()
            .is_some_and(|transfer| transfer != "basic")
        {
            return Err(self.refused(format!(
                "it chose the `{}` transfer, and only `basic` is spoken",
                response.transfer.unwrap_or_default()
            )));
        }
        Ok(response.objects)
    }

    fn content_the_remote_answered_for<'batch>(
        &self,
        object: &LfsBatchResponseObject,
        batch: &'batch [StoredProjectContent],
    ) -> Result<&'batch StoredProjectContent, ProjectRepositoryRefusal> {
        batch
            .iter()
            .find(|content| content.content_sha256 == object.oid)
            .ok_or_else(|| {
                self.refused(format!(
                    "it answered for {}, which was not asked for",
                    object.oid
                ))
            })
    }

    fn headers_of_a_transfer(&self, action: &LfsTransferAction) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = action
            .header
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let names_its_own_authorization = headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
        let is_at_the_endpoints_origin = origin_of_a_remote_url(&action.href).is_some()
            && origin_of_a_remote_url(&action.href)
                == origin_of_a_remote_url(&self.lfs_endpoint_url);
        if let Some(bearer_token) = &self.bearer_token
            && !names_its_own_authorization
            && is_at_the_endpoints_origin
        {
            headers.push(("Authorization".to_owned(), format!("Bearer {bearer_token}")));
        }
        headers
    }

    fn refused(&self, why: String) -> ProjectRepositoryRefusal {
        ProjectRepositoryRefusal::ContentRemoteRefused {
            url: self.lfs_endpoint_url.clone(),
            why,
        }
    }
}

/// Git LFS's default endpoint of the Git remote `url`: `<url>/info/lfs` for a URL ending `.git`,
/// `<url>.git/info/lfs` otherwise.
fn the_default_lfs_endpoint_of(url: &str) -> String {
    let url = url.trim_end_matches('/');
    if url.ends_with(".git") {
        format!("{url}/info/lfs")
    } else {
        format!("{url}.git/info/lfs")
    }
}

/// The `url` key of the `[lfs]` section of a Git config file's bytes.
fn the_lfs_url_an_lfs_config_names(lfs_config: &[u8]) -> Option<String> {
    let mut in_the_lfs_section = false;
    for line in String::from_utf8_lossy(lfs_config).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[') {
            let section_name = section.trim_end_matches(']').trim();
            in_the_lfs_section = section_name.eq_ignore_ascii_case("lfs");
            continue;
        }
        if !in_the_lfs_section {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            let value = value.trim().trim_matches('"').trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum LfsOperation {
    Upload,
    Download,
}

#[derive(Serialize)]
struct LfsBatchRequest {
    operation: LfsOperation,
    transfers: Vec<&'static str>,
    hash_algo: &'static str,
    objects: Vec<LfsObjectNamed>,
}

#[derive(Serialize)]
struct LfsObjectNamed {
    oid: String,
    size: u64,
}

#[derive(Deserialize)]
struct LfsBatchResponse {
    #[serde(default)]
    transfer: Option<String>,
    objects: Vec<LfsBatchResponseObject>,
}

#[derive(Deserialize)]
struct LfsBatchResponseObject {
    oid: String,
    #[serde(default)]
    actions: BTreeMap<String, LfsTransferAction>,
    #[serde(default)]
    error: Option<LfsObjectError>,
}

#[derive(Deserialize)]
struct LfsTransferAction {
    href: String,
    #[serde(default)]
    header: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct LfsObjectError {
    code: u16,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_remote(url: &str) -> ProjectHistoryRemote {
        ProjectHistoryRemote {
            url: url.to_owned(),
            bearer_token: Some("history-token".to_owned()),
            content_remote: None,
        }
    }

    #[test]
    fn the_default_endpoint_follows_git_lfs_and_takes_the_history_token() {
        let content_remote = ProjectContentRemote::beside_the_history_remote(
            &history_remote("https://github.com/team/arm.git"),
            None,
        )
        .unwrap();
        assert_eq!(
            content_remote.lfs_endpoint_url,
            "https://github.com/team/arm.git/info/lfs"
        );
        assert_eq!(
            content_remote.bearer_token.as_deref(),
            Some("history-token")
        );
        assert_eq!(
            the_default_lfs_endpoint_of("https://host/team/arm/"),
            "https://host/team/arm.git/info/lfs"
        );
    }

    #[test]
    fn an_lfs_config_on_another_origin_names_the_endpoint_and_is_sent_no_history_token() {
        let lfs_config = b"# the team's store\n[core]\n\turl = https://wrong\n[lfs]\n\turl = \"https://content.tatolab.example/team/arm\"\n";
        let content_remote = ProjectContentRemote::beside_the_history_remote(
            &history_remote("https://abc.artifacts.cloudflare.net/git/team/arm.git"),
            Some(lfs_config),
        )
        .unwrap();
        assert_eq!(
            content_remote.lfs_endpoint_url,
            "https://content.tatolab.example/team/arm"
        );
        assert_eq!(content_remote.bearer_token, None);
    }

    #[test]
    fn a_content_remote_the_history_remote_names_wins_and_a_directory_has_none() {
        let mut named = history_remote("https://abc.artifacts.cloudflare.net/git/team/arm.git");
        named.content_remote = Some(ProjectContentRemote {
            lfs_endpoint_url: "https://content.example/arm".to_owned(),
            bearer_token: Some("content-token".to_owned()),
        });
        let content_remote =
            ProjectContentRemote::beside_the_history_remote(&named, Some(b"[lfs]\nurl = x\n"))
                .unwrap();
        assert_eq!(
            content_remote.bearer_token.as_deref(),
            Some("content-token")
        );
        assert!(
            ProjectContentRemote::beside_the_history_remote(
                &history_remote("/projects/arm/.tatolab/history.git"),
                None
            )
            .is_none()
        );
    }

    #[test]
    fn a_transfer_elsewhere_or_naming_its_own_authorization_is_sent_no_token() {
        let content_remote = ProjectContentRemote {
            lfs_endpoint_url: "https://host/arm.git/info/lfs".to_owned(),
            bearer_token: Some("t".to_owned()),
        };
        let at = |href: &str, header: &[(&str, &str)]| LfsTransferAction {
            href: href.to_owned(),
            header: header
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        };
        let authorization_of = |action: &LfsTransferAction| {
            content_remote
                .headers_of_a_transfer(action)
                .into_iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value)
        };
        assert_eq!(
            authorization_of(&at("https://host/objects/abc", &[])).as_deref(),
            Some("Bearer t")
        );
        assert_eq!(
            authorization_of(&at("https://bucket.s3.example/abc?sig=1", &[])),
            None
        );
        assert_eq!(
            authorization_of(&at(
                "https://host/objects/abc",
                &[("Authorization", "RemoteAuth x")]
            ))
            .as_deref(),
            Some("RemoteAuth x")
        );
    }
}
