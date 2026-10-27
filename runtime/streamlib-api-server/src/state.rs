// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared HTTP state, OpenAPI document, and request/response wire types.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_relay_adapter_sdk::{
    RelayAdapterCatalogEntry, RelayAdapterSettingField, RelayInstancesOfThisMachine,
    RelaySettingFieldType,
};
use utoipa::OpenApi;

/// Shared HTTP handler state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    pub relay_instances: Arc<dyn RelayInstancesOfThisMachine>,
    pub openapi: utoipa::openapi::OpenApi,
}

/// The relay-adapter catalog: every adapter compiled into the runtime.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RelayAdapterCatalogResponse {
    /// Each adapter, as `add_relay` takes its kind.
    pub adapters: Vec<RelayAdapterCatalogResponseAdapter>,
}

/// One adapter of a [`RelayAdapterCatalogResponse`].
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RelayAdapterCatalogResponseAdapter {
    /// The adapter's kind, as `add_relay` takes it.
    pub kind: String,
    /// The adapter's name as people read it, such as `moq.pro`.
    pub display_name: String,
    /// The adapter's icon as a `data:` URI an `<img>` shows.
    pub display_icon_data_uri: String,
    /// What the adapter joins.
    pub doc: String,
    /// The settings an instance takes, each as the field a form draws.
    pub settings: Vec<RelayAdapterCatalogResponseSetting>,
}

impl From<RelayAdapterCatalogEntry> for RelayAdapterCatalogResponseAdapter {
    fn from(
        RelayAdapterCatalogEntry {
            kind,
            display_name,
            display_icon_data_uri,
            doc,
            settings,
        }: RelayAdapterCatalogEntry,
    ) -> Self {
        Self {
            kind,
            display_name,
            display_icon_data_uri,
            doc,
            settings: settings.into_iter().map(Into::into).collect(),
        }
    }
}

/// One setting of a [`RelayAdapterCatalogResponseAdapter`].
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RelayAdapterCatalogResponseSetting {
    /// The key `add_relay` and `configure_relay` take it under in `settings`.
    pub name: String,
    /// How a person enters it: the control a form draws it with.
    #[serde(rename = "type")]
    pub field_type: RelayAdapterCatalogResponseSettingFieldType,
    /// The values a `select` takes; present for a `select` alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Whether it is kept in the OS secret store and listed by `list_relays`
    /// only as set or unset.
    pub secret: bool,
    /// Whether an instance's settings must carry it.
    pub required: bool,
    /// What it is.
    pub doc: String,
    /// Its value when an instance's settings leave it out, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub default: Option<serde_json::Value>,
}

/// The field type of a [`RelayAdapterCatalogResponseSetting`].
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RelayAdapterCatalogResponseSettingFieldType {
    /// One line of text.
    String,
    /// A URL.
    Url,
    /// A number.
    Number,
    /// On or off.
    Boolean,
    /// One of `options`.
    Select,
    /// A list of text values.
    StringList,
    /// A file's text, picked as a file or pasted.
    File,
}

impl From<RelayAdapterSettingField> for RelayAdapterCatalogResponseSetting {
    fn from(
        RelayAdapterSettingField {
            name,
            field_type,
            secret,
            required,
            doc,
            default,
        }: RelayAdapterSettingField,
    ) -> Self {
        let (field_type, options) = match field_type {
            RelaySettingFieldType::String => {
                (RelayAdapterCatalogResponseSettingFieldType::String, None)
            }
            RelaySettingFieldType::Url => (RelayAdapterCatalogResponseSettingFieldType::Url, None),
            RelaySettingFieldType::Number => {
                (RelayAdapterCatalogResponseSettingFieldType::Number, None)
            }
            RelaySettingFieldType::Boolean => {
                (RelayAdapterCatalogResponseSettingFieldType::Boolean, None)
            }
            RelaySettingFieldType::Select { options } => (
                RelayAdapterCatalogResponseSettingFieldType::Select,
                Some(options),
            ),
            RelaySettingFieldType::StringList => (
                RelayAdapterCatalogResponseSettingFieldType::StringList,
                None,
            ),
            RelaySettingFieldType::File => {
                (RelayAdapterCatalogResponseSettingFieldType::File, None)
            }
        };
        Self {
            name,
            field_type,
            options,
            secret,
            required,
            doc,
            default,
        }
    }
}

impl RelayAdapterCatalogResponse {
    /// The catalog `relay_instances` serves.
    pub(crate) fn of(relay_instances: &dyn RelayInstancesOfThisMachine) -> Self {
        Self {
            adapters: relay_instances
                .relay_adapter_catalog()
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

/// The `stream` query parameter of a route that reads one stream or, without
/// it, every loaded stream.
#[derive(Deserialize)]
pub(crate) struct OptionalStreamSelectionQuery {
    /// The loaded stream the call names; absent reads every loaded stream.
    pub stream: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ErrorResponse {
    /// Error message
    pub error: String,
}

// ============================================================================
// OpenAPI Documentation
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    info(
        title = "StreamLib Runtime API",
        version = "0.1.0",
        description = "Observation API for the machine's runtime: the graph of each stream it loads, the node types each can add, and its event stream. `/api/relay-adapters` lists the relay adapters a relay instance is added through, each with its settings schema. `/api/graph` and `/api/registry` take an optional `stream` query parameter naming one loaded stream, and read every loaded stream without it. A stream's graph is defined by its code, so this API does not mutate it.",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "relays", description = "The relay adapters compiled into the runtime"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
pub(crate) struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_served_setting_is_the_relay_adapter_sdks_field_as_it_serializes() {
        for field_type in [
            RelaySettingFieldType::String,
            RelaySettingFieldType::Url,
            RelaySettingFieldType::Number,
            RelaySettingFieldType::Boolean,
            RelaySettingFieldType::Select {
                options: vec!["us".to_string(), "eu".to_string()],
            },
            RelaySettingFieldType::StringList,
            RelaySettingFieldType::File,
        ] {
            let field = RelayAdapterSettingField {
                name: "setting".to_string(),
                field_type,
                secret: true,
                required: false,
                doc: "A setting.".to_string(),
                default: Some(serde_json::json!(4443)),
            };
            assert_eq!(
                serde_json::to_value(RelayAdapterCatalogResponseSetting::from(field.clone()))
                    .unwrap(),
                serde_json::to_value(&field).unwrap()
            );
        }
    }
}
