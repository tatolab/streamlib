// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a moq.pro relay instance's settings may say.

use serde::Deserialize;
use streamlib_relay_adapter_sdk::RelaySecretSetting;
use streamlib_relay_adapter_sdk::url::Url;

/// moq.pro's one relay cluster, every project served at its path under it;
/// `https://cdn.moq.dev` names the same cluster.
pub const MOQPRO_RELAY_URL: &str = "https://cdn.moq.pro";

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
    type Error = MoqProProjectIdRefusal;

    fn try_from(project_id: String) -> Result<Self, Self::Error> {
        let trimmed = project_id.trim();
        if trimmed.is_empty() || trimmed.contains('/') {
            return Err(MoqProProjectIdRefusal::ProjectIdNotOnePathSegment { project_id });
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
}

/// Why a moq.pro instance's project id was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoqProProjectIdRefusal {
    /// The project id is empty or names more than one path segment.
    ProjectIdNotOnePathSegment { project_id: String },
}

impl std::fmt::Display for MoqProProjectIdRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProjectIdNotOnePathSegment { project_id } => write!(
                f,
                "`project_id` `{project_id}` is not a moq.pro project id: one path segment, \
                 non-empty, with no `/`"
            ),
        }
    }
}

impl MoqProRelayInstanceSettings {
    /// The URL the project's relay is dialed at: moq.pro's relay URL with the
    /// project id as its one path segment.
    pub fn the_projects_dial_url(&self) -> Url {
        let mut dial_url = Url::parse(MOQPRO_RELAY_URL).expect("moq.pro's relay URL is a URL");
        dial_url
            .path_segments_mut()
            .expect("moq.pro's relay URL is an `https://` URL, which has a path")
            .pop_if_empty()
            .push(self.project_id.as_str());
        dial_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(project_id: &str) -> MoqProRelayInstanceSettings {
        settings_read(project_id).unwrap()
    }

    fn settings_read(project_id: &str) -> Result<MoqProRelayInstanceSettings, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "project_id": project_id,
            "signing_key": "{}",
        }))
    }

    #[test]
    fn the_project_is_dialed_at_its_path_on_moq_pros_relay() {
        assert_eq!(
            settings("pb1xchshwmyjy").the_projects_dial_url().as_str(),
            "https://cdn.moq.pro/pb1xchshwmyjy"
        );
    }

    #[test]
    fn a_project_id_that_is_not_one_segment_is_refused() {
        for project_id in ["", "a/b", " "] {
            let refused = settings_read(project_id).unwrap_err().to_string();
            assert!(refused.contains("is not a moq.pro project id"), "{refused}");
        }
    }

    #[test]
    fn a_project_id_a_url_reserves_a_character_of_is_dialed_as_one_path_segment() {
        assert_eq!(
            settings("pb1?x#y").the_projects_dial_url().as_str(),
            "https://cdn.moq.pro/pb1%3Fx%23y"
        );
    }

    #[test]
    fn a_project_id_is_trimmed_once_and_read_trimmed_everywhere() {
        let settings = settings(" pb1test ");
        assert_eq!(settings.project_id.as_str(), "pb1test");
        assert_eq!(
            settings.the_projects_dial_url().as_str(),
            "https://cdn.moq.pro/pb1test"
        );
    }

    #[test]
    fn a_setting_it_does_not_take_is_refused_naming_it() {
        for setting_it_does_not_take in ["root", "relay_url"] {
            let mut settings = serde_json::json!({"project_id": "pb1", "signing_key": "{}"});
            settings[setting_it_does_not_take] = serde_json::json!("https://cdn.moq.dev");
            let refused = serde_json::from_value::<MoqProRelayInstanceSettings>(settings)
                .unwrap_err()
                .to_string();
            assert!(refused.contains(setting_it_does_not_take), "{refused}");
        }
    }
}
