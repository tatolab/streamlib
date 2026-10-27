// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! moq.pro's hosted MoQ relay. Each instance is one moq.pro project, dialed
//! at the project's path on moq.pro's relay with a JWT the adapter signs with
//! the project's key: rooted at the project, granting publish to this
//! machine's own broadcasts alone, and renewed before it expires. The key
//! mints read and write for every broadcast of the project, so it lives in the
//! OS secret store and nowhere else; a token is never written down.

mod moqpro_relay_instance_settings;
mod moqpro_relay_token;

use streamlib_relay_adapter_sdk::url::Url;
use streamlib_relay_adapter_sdk::{
    BroadcastPath, JoinedRelay, MachineName, OutputPortExposureLevel, RelayAdapter,
    RelayAdapterCatalogEntry, RelayAdapterInstanceMaker, RelayAdapterKind, RelayAdapterRefusal,
    RelayConnectionState, RelayConnectionStateReceiver, RelayDialCredential, RelayInstanceId,
    RelaySettingsJsonSchema, relay_instance_settings_from_json,
};
use tokio::sync::watch;

pub use moqpro_relay_instance_settings::{
    MOQPRO_RELAY_URL, MoqProProjectId, MoqProRelayInstanceSettings, MoqProRelaySettingsRefusal,
};
pub use moqpro_relay_token::{
    MOQPRO_PUBLISH_TOKEN_LIFETIME, MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER, MoqProProjectSigningKey,
    MoqProSigningKeyRefusal,
};

/// The adapter kind every moq.pro relay instance is joined through.
pub const MOQPRO_RELAY_ADAPTER_KIND: RelayAdapterKind = RelayAdapterKind("moqpro");

/// A moq.pro relay serves public ports alone.
const LEVELS_A_MOQPRO_RELAY_SERVES: [OutputPortExposureLevel; 1] =
    [OutputPortExposureLevel::Public];

/// What a joined instance mints its tokens with.
struct JoinedMoqProProject {
    project_id: MoqProProjectId,
    signing_key: MoqProProjectSigningKey,
}

/// One moq.pro project.
pub struct MoqProRelayAdapter {
    relay_id: RelayInstanceId,
    machine: MachineName,
    connection_state: watch::Sender<RelayConnectionState>,
    joined: Option<JoinedMoqProProject>,
}

impl MoqProRelayAdapter {
    /// The instance `relay_id` on `machine`, not yet joined.
    pub fn new(relay_id: RelayInstanceId, machine: MachineName) -> Self {
        Self {
            relay_id,
            machine,
            connection_state: watch::channel(RelayConnectionState::NotJoined).0,
            joined: None,
        }
    }

    fn settings_refused(&self, why: impl std::fmt::Display) -> RelayAdapterRefusal {
        RelayAdapterRefusal::settings_refused(&self.relay_id, why)
    }

    fn mint_a_publish_credential(
        &self,
        joined: &JoinedMoqProProject,
    ) -> Result<RelayDialCredential, MoqProSigningKeyRefusal> {
        joined
            .signing_key
            .mint_a_publish_credential_for_the_machine(&joined.project_id, &self.machine)
    }
}

impl RelayAdapter for MoqProRelayAdapter {
    fn kind() -> RelayAdapterKind {
        MOQPRO_RELAY_ADAPTER_KIND
    }

    fn doc() -> &'static str {
        "A moq.pro project, dialed at its path on moq.pro's relay with a token signed by the \
         project's key, which is kept as a secret. Serves public ports, under the project."
    }

    fn display_name() -> &'static str {
        "moq.pro"
    }

    fn display_icon_svg() -> &'static str {
        include_str!("moqpro_relay_adapter_display_icon.svg")
    }

    fn relay_id(&self) -> &RelayInstanceId {
        &self.relay_id
    }

    fn settings_schema() -> serde_json::Value {
        MoqProRelayInstanceSettings::processor_config_schema_document()
    }

    fn join(&mut self, settings: serde_json::Value) -> Result<JoinedRelay, RelayAdapterRefusal> {
        let settings: MoqProRelayInstanceSettings =
            relay_instance_settings_from_json(&self.relay_id, settings)?;
        let dial_url: Url = settings.the_projects_dial_url();
        let signing_key = MoqProProjectSigningKey::read_from(settings.signing_key.expose_secret())
            .map_err(|refusal| self.settings_refused(refusal))?;
        let joined = JoinedMoqProProject {
            project_id: settings.project_id,
            signing_key,
        };
        self.mint_a_publish_credential(&joined)
            .map_err(|refusal| self.settings_refused(refusal))?;
        self.joined = Some(joined);
        self.connection_state
            .send_replace(RelayConnectionState::Joined);
        Ok(JoinedRelay { dial_url })
    }

    fn leave(&mut self) {
        self.joined = None;
        self.connection_state
            .send_replace(RelayConnectionState::NotJoined);
    }

    fn levels_served(&self) -> &[OutputPortExposureLevel] {
        &LEVELS_A_MOQPRO_RELAY_SERVES
    }

    fn broadcast_root(&self, machine: &MachineName) -> BroadcastPath {
        BroadcastPath::of_the_machine_under_the_root("", machine)
    }

    fn credential_for_dial(&self) -> Option<RelayDialCredential> {
        let joined = self.joined.as_ref()?;
        self.mint_a_publish_credential(joined)
            .inspect_err(|refusal| {
                tracing::error!(relay = %self.relay_id, "no publish token is minted: {refusal}");
            })
            .ok()
    }

    fn events(&self) -> RelayConnectionStateReceiver {
        self.connection_state.subscribe()
    }
}

/// Makes moq.pro relay instances for one machine, as many as are added.
pub struct MoqProRelayAdapterInstanceMaker {
    machine: MachineName,
}

impl MoqProRelayAdapterInstanceMaker {
    /// The maker of `machine`'s instances, whose tokens grant its broadcasts.
    pub fn for_the_machine(machine: MachineName) -> Self {
        Self { machine }
    }
}

impl RelayAdapterInstanceMaker for MoqProRelayAdapterInstanceMaker {
    fn catalog_entry(&self) -> RelayAdapterCatalogEntry {
        RelayAdapterCatalogEntry::of_the_adapter::<MoqProRelayAdapter>()
    }

    fn make_the_instance(
        &self,
        relay_id: &RelayInstanceId,
    ) -> Result<Box<dyn RelayAdapter>, RelayAdapterRefusal> {
        Ok(Box::new(MoqProRelayAdapter::new(
            relay_id.clone(),
            self.machine.clone(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moq_auth::{Algorithm, Key, KeyId, Pattern};
    use std::time::{Duration, Instant};
    use streamlib_relay_adapter_sdk::secrecy::ExposeSecret;

    fn a_project_key() -> Key {
        Key::generate(
            Algorithm::ES256,
            Some(KeyId::decode("pb1test-default").unwrap()),
        )
        .unwrap()
    }

    fn the_machine() -> MachineName {
        MachineName::from_a_hostname("desk").unwrap()
    }

    fn an_instance() -> MoqProRelayAdapter {
        MoqProRelayAdapter::new(RelayInstanceId("moqpro-1".to_string()), the_machine())
    }

    fn settings_with(key: &Key) -> serde_json::Value {
        serde_json::json!({
            "project_id": "pb1test",
            "signing_key": serde_json::to_string(key).unwrap(),
        })
    }

    #[test]
    fn the_schema_takes_the_project_and_the_key_as_a_secret() {
        let entry = MoqProRelayAdapterInstanceMaker::for_the_machine(the_machine()).catalog_entry();
        assert_eq!(entry.kind, "moqpro");
        let mut properties = entry.settings_schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        properties.sort();
        assert_eq!(properties, ["project_id", "signing_key"]);
        assert_eq!(entry.secret_setting_names(), ["signing_key"]);
        let mut required = entry.settings_schema["required"]
            .as_array()
            .unwrap()
            .clone();
        required.sort_by_key(|name| name.to_string());
        assert_eq!(
            required,
            [
                serde_json::json!("project_id"),
                serde_json::json!("signing_key")
            ]
        );
    }

    #[test]
    fn it_dials_the_projects_path_with_a_publish_token_for_the_machine_renewed_before_a_day() {
        let key = a_project_key();
        let mut adapter = an_instance();
        let joined = adapter.join(settings_with(&key)).unwrap();
        assert_eq!(joined.dial_url.as_str(), "https://cdn.moq.dev/pb1test");
        assert_eq!(*adapter.events().borrow(), RelayConnectionState::Joined);

        let credential = adapter.credential_for_dial().unwrap();
        let renew_by = credential.renew_by().unwrap();
        assert!(renew_by <= Instant::now() + MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER);
        assert!(
            renew_by
                > Instant::now() + MOQPRO_PUBLISH_TOKEN_RENEWED_AFTER - Duration::from_secs(60)
        );

        let dial_url = credential.dial_url_carrying_the_credential(&joined.dial_url);
        let dial_url = Url::parse(dial_url.expose_secret()).unwrap();
        assert_eq!(dial_url.path(), "/pb1test");
        let (parameter, token) = dial_url.query_pairs().next().unwrap();
        assert_eq!(parameter, "jwt");
        let claims = key.verify(&token).unwrap();
        assert_eq!(claims.root, "pb1test");
        assert_eq!(
            claims.publish,
            ["desk/**".parse::<Pattern>().unwrap()]
                .into_iter()
                .collect()
        );
        assert_eq!(adapter.broadcast_root(&the_machine()).as_str(), "desk");
        assert_eq!(adapter.levels_served(), [OutputPortExposureLevel::Public]);
    }

    #[test]
    fn each_ask_mints_a_fresh_token() {
        let key = a_project_key();
        let mut adapter = an_instance();
        let joined = adapter.join(settings_with(&key)).unwrap();
        let first = adapter
            .credential_for_dial()
            .unwrap()
            .dial_url_carrying_the_credential(&joined.dial_url);
        std::thread::sleep(Duration::from_millis(1100));
        let second = adapter
            .credential_for_dial()
            .unwrap()
            .dial_url_carrying_the_credential(&joined.dial_url);
        assert_ne!(first.expose_secret(), second.expose_secret());
    }

    #[test]
    fn a_key_or_a_setting_it_cannot_use_is_refused_naming_the_relay_and_no_key_material() {
        let key = a_project_key();
        let jwk = serde_json::to_value(&key).unwrap();
        let private_scalar = jwk["d"].as_str().unwrap().to_string();
        let public_key = key.to_public().unwrap();
        for (settings, expected) in [
            (
                serde_json::json!({"project_id": "pb1test", "signing_key": format!("{{{private_scalar}")}),
                "is not a JWK",
            ),
            (
                serde_json::json!({"project_id": "pb1test", "signing_key": serde_json::to_string(&public_key).unwrap()}),
                "cannot sign",
            ),
            (
                serde_json::json!({"project_id": "pb1/test", "signing_key": serde_json::to_string(&key).unwrap()}),
                "project_id",
            ),
            (
                serde_json::json!({"project_id": "pb1test", "signing_key": serde_json::to_string(&key).unwrap(), "relay_url": "https://cdn.moq.pro"}),
                "relay_url",
            ),
        ] {
            let mut adapter = an_instance();
            let refused = adapter.join(settings).err().unwrap().to_string();
            assert!(refused.contains("`moqpro-1`"), "{refused}");
            assert!(refused.contains(expected), "{refused}");
            assert!(!refused.contains(&private_scalar), "{refused}");
            assert!(adapter.credential_for_dial().is_none());
        }
    }

    #[test]
    fn a_project_id_with_stray_space_roots_its_tokens_at_the_path_it_dials() {
        let key = a_project_key();
        let mut settings = settings_with(&key);
        settings["project_id"] = serde_json::json!(" pb1test ");
        let mut adapter = an_instance();
        let joined = adapter.join(settings).unwrap();
        assert_eq!(joined.dial_url.as_str(), "https://cdn.moq.dev/pb1test");
        let dial_url = adapter
            .credential_for_dial()
            .unwrap()
            .dial_url_carrying_the_credential(&joined.dial_url);
        let dial_url = Url::parse(dial_url.expose_secret()).unwrap();
        let (_, token) = dial_url.query_pairs().next().unwrap();
        assert_eq!(key.verify(&token).unwrap().root, "pb1test");
    }

    #[test]
    fn leaving_forgets_the_key() {
        let mut adapter = an_instance();
        adapter.join(settings_with(&a_project_key())).unwrap();
        adapter.leave();
        assert!(adapter.credential_for_dial().is_none());
        assert_eq!(*adapter.events().borrow(), RelayConnectionState::NotJoined);
    }

    #[test]
    fn the_catalog_entry_names_moq_pro_for_people_and_its_icon_is_an_svg_document() {
        let catalog_entry = RelayAdapterCatalogEntry::of_the_adapter::<MoqProRelayAdapter>();
        assert_eq!(catalog_entry.display_name, "moq.pro");
        let display_icon_svg = MoqProRelayAdapter::display_icon_svg();
        assert!(display_icon_svg.contains("<svg "));
        assert!(display_icon_svg.trim_end().ends_with("</svg>"));
    }
}
