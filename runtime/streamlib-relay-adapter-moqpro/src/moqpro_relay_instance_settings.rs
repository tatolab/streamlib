// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a moq.pro relay instance's settings may say.

use serde::Deserialize;
use streamlib_relay_adapter_sdk::RelaySecretSetting;
use streamlib_relay_adapter_sdk::url::Url;

/// The relays moq.pro serves its projects at, a project's path under it.
pub const MOQPRO_DEFAULT_RELAY_URL: &str = "https://cdn.moq.dev";

/// A moq.pro project's id, trimmed and one path segment: the root every token
/// is minted under and the path the relay is dialed at.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct MoqProProjectId(String);

impl MoqProProjectId {
    /// The id, as moq.pro names it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MoqProProjectId {
    type Error = MoqProRelaySettingsRefusal;

    fn try_from(project_id: String) -> Result<Self, Self::Error> {
        let trimmed = project_id.trim();
        if trimmed.is_empty() || trimmed.contains('/') {
            return Err(MoqProRelaySettingsRefusal::ProjectIdNotOnePathSegment { project_id });
        }
        Ok(Self(trimmed.to_string()))
    }
}

/// A moq.pro relay instance's settings.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoqProRelayInstanceSettings {
    /// The project's id as moq.pro names it: the root every token is minted
    /// under and the path the relay is dialed at.
    #[schemars(with = "String")]
    pub project_id: MoqProProjectId,
    /// The project's signing key, the JWK moq.pro gave for it, as JSON or
    /// base64url JSON. It mints read and write for every broadcast of the
    /// project.
    pub signing_key: RelaySecretSetting,
    /// The relays the project is served at; absent, `https://cdn.moq.dev`.
    #[serde(default)]
    pub relay_url: Option<String>,
}

/// Why a moq.pro instance's project or relay URL was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoqProRelaySettingsRefusal {
    /// The project id is empty or names more than one path segment.
    ProjectIdNotOnePathSegment { project_id: String },
    /// The relay URL is not `https://`, or carries a query or a fragment.
    RelayUrlNotAnHttpsUrl { relay_url: String },
}

impl std::fmt::Display for MoqProRelaySettingsRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProjectIdNotOnePathSegment { project_id } => write!(
                f,
                "`project_id` `{project_id}` is not a moq.pro project id: one path segment, \
                 non-empty, with no `/`"
            ),
            Self::RelayUrlNotAnHttpsUrl { relay_url } => write!(
                f,
                "`relay_url` `{relay_url}` is not an `https://` URL with no query or fragment"
            ),
        }
    }
}

impl MoqProRelayInstanceSettings {
    /// The URL the project's relay is dialed at: the relay URL with the
    /// project id as its last path segment.
    pub fn the_projects_dial_url(&self) -> Result<Url, MoqProRelaySettingsRefusal> {
        let relay_url = self
            .relay_url
            .as_deref()
            .unwrap_or(MOQPRO_DEFAULT_RELAY_URL);
        let not_an_https_url = || MoqProRelaySettingsRefusal::RelayUrlNotAnHttpsUrl {
            relay_url: relay_url.to_string(),
        };
        let mut dial_url = Url::parse(relay_url).map_err(|_| not_an_https_url())?;
        if dial_url.scheme() != "https"
            || dial_url.query().is_some()
            || dial_url.fragment().is_some()
        {
            return Err(not_an_https_url());
        }
        dial_url
            .path_segments_mut()
            .map_err(|_| not_an_https_url())?
            .pop_if_empty()
            .push(self.project_id.as_str());
        Ok(dial_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(project_id: &str, relay_url: Option<&str>) -> MoqProRelayInstanceSettings {
        settings_read(project_id, relay_url).unwrap()
    }

    fn settings_read(
        project_id: &str,
        relay_url: Option<&str>,
    ) -> Result<MoqProRelayInstanceSettings, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "project_id": project_id,
            "signing_key": "{}",
            "relay_url": relay_url,
        }))
    }

    #[test]
    fn the_project_is_dialed_at_its_path_on_moq_pros_relays_by_default() {
        assert_eq!(
            settings("pb1xchshwmyjy", None)
                .the_projects_dial_url()
                .unwrap()
                .as_str(),
            "https://cdn.moq.dev/pb1xchshwmyjy"
        );
    }

    #[test]
    fn a_project_id_that_is_not_one_segment_or_a_url_that_is_not_https_is_refused() {
        for project_id in ["", "a/b", " "] {
            let refused = settings_read(project_id, None).unwrap_err().to_string();
            assert!(refused.contains("is not a moq.pro project id"), "{refused}");
        }
        for relay_url in [
            "http://cdn.moq.dev",
            "cdn.moq.dev",
            "https://cdn.moq.dev/?jwt=x",
        ] {
            assert!(matches!(
                settings("pb1", Some(relay_url)).the_projects_dial_url(),
                Err(MoqProRelaySettingsRefusal::RelayUrlNotAnHttpsUrl { .. })
            ));
        }
    }

    #[test]
    fn a_project_id_is_trimmed_once_and_read_trimmed_everywhere() {
        let settings = settings(" pb1test ", None);
        assert_eq!(settings.project_id.as_str(), "pb1test");
        assert_eq!(
            settings.the_projects_dial_url().unwrap().as_str(),
            "https://cdn.moq.dev/pb1test"
        );
    }

    #[test]
    fn a_setting_it_does_not_take_is_refused_naming_it() {
        let refused = serde_json::from_value::<MoqProRelayInstanceSettings>(serde_json::json!({
            "project_id": "pb1", "signing_key": "{}", "root": "pb1",
        }))
        .unwrap_err()
        .to_string();
        assert!(refused.contains("root"), "{refused}");
    }
}
