// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The machine's relays, managed one call at a time: `relays`, `relay add`, `relay set` and
//! `relay rm`, each the local API tool of the same purpose, and the one line or table each
//! prints. A secret is never an argument: `relay set` reads it once from a file or stdin.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{ArgGroup, Subcommand};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AddRelayToolResult, ConfigureRelayToolResult, ListRelaysToolResult, ListedRelayConnectionState,
    ListedRelayInstance, RemoveRelayToolResult,
};

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::OBSERVATION_VERB_TOOL_CALL_TIMEOUT;
use crate::machine_runtime_local_api_socket::call_one_tool_of_the_running_runtime;
use crate::verb_standard_output::write_verb_standard_output;

/// How long `relay add`, `relay set` and `relay rm` wait for the runtime: a join may reach its
/// provider before it answers.
const RELAY_ACTION_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// What `relays` prints when the runtime holds no relay.
const NO_RELAYS_IN_THIS_RUNTIME_LINE: &str = "no relays\n";

/// `tatolab relay`'s verbs.
#[derive(Subcommand)]
pub(crate) enum RelayVerb {
    /// Add a relay through an adapter, and join it.
    #[command(
        long_about = "The settings file is a JSON object of the adapter's settings, as the \
                      relay-adapter catalog's schema describes them; a secret among them is kept \
                      in the OS secret store, never in relays.json. Prints the id the runtime \
                      gave the relay."
    )]
    Add {
        /// The adapter's kind, as the relay-adapter catalog lists it.
        adapter: String,
        /// A JSON object of the relay's settings.
        #[arg(long = "settings-file", value_name = "PATH")]
        settings_file: Option<PathBuf>,
    },
    /// Change a relay's settings, or one of its secrets, and join it again.
    #[command(
        long_about = "Each KEY=VALUE replaces one setting; VALUE is read as JSON when it parses \
                      (4500, true, null — null returns the setting to its default) and as text \
                      otherwise. A secret is never an argument: --secret FIELD reads it once from \
                      --from-file PATH or --from-stdin.",
        group(ArgGroup::new("secret_source").args(["from_file", "from_stdin"]))
    )]
    Set {
        /// The relay, as `tatolab relays` names it.
        relay_id: String,
        /// Settings to replace, each KEY=VALUE.
        #[arg(value_name = "KEY=VALUE")]
        settings: Vec<String>,
        /// The secret setting to replace.
        #[arg(long = "secret", value_name = "FIELD", requires = "secret_source")]
        secret: Option<String>,
        /// Read the secret from this file.
        #[arg(long = "from-file", value_name = "PATH", requires = "secret")]
        from_file: Option<PathBuf>,
        /// Read the secret from standard input.
        #[arg(long = "from-stdin", requires = "secret")]
        from_stdin: bool,
    },
    /// Leave a relay and forget it and its secrets.
    Rm {
        /// The relay, as `tatolab relays` names it.
        relay_id: String,
    },
}

/// Run one of `tatolab relay`'s verbs.
pub(crate) fn run_relay_verb(relay_verb: RelayVerb) -> Result<u8, TatolabCommandFailure> {
    match relay_verb {
        RelayVerb::Add {
            adapter,
            settings_file,
        } => add_relay(&adapter, settings_file.as_deref()),
        RelayVerb::Set {
            relay_id,
            settings,
            secret,
            from_file,
            from_stdin,
        } => {
            let secret = match (secret, from_file, from_stdin) {
                (Some(field), Some(path), _) => Some((field, SecretSource::File(path))),
                (Some(field), None, true) => Some((field, SecretSource::StandardInput)),
                _ => None,
            };
            configure_relay(&relay_id, &settings, secret)
        }
        RelayVerb::Rm { relay_id } => remove_relay(&relay_id),
    }
}

/// Where `relay set --secret` reads its secret from.
enum SecretSource {
    File(PathBuf),
    StandardInput,
}

impl SecretSource {
    /// The secret, read once, its one trailing line ending dropped.
    fn read_the_secret(&self) -> Result<String, TatolabCommandFailure> {
        let mut secret = match self {
            SecretSource::File(path) => std::fs::read_to_string(path).map_err(|not_read| {
                TatolabCommandFailure::refused(format!(
                    "the secret cannot be read from {}: {not_read}",
                    path.display()
                ))
            })?,
            SecretSource::StandardInput => {
                let mut secret = String::new();
                std::io::stdin()
                    .read_to_string(&mut secret)
                    .map_err(|not_read| {
                        TatolabCommandFailure::refused(format!(
                            "the secret cannot be read from standard input: {not_read}"
                        ))
                    })?;
                secret
            }
        };
        for line_ending in ["\r\n", "\n"] {
            if secret.ends_with(line_ending) {
                secret.truncate(secret.len() - line_ending.len());
                break;
            }
        }
        if secret.is_empty() {
            return Err(TatolabCommandFailure::refused(
                "the secret read is empty".to_owned(),
            ));
        }
        Ok(secret)
    }
}

fn call_one_relay_tool<ToolResult: DeserializeOwned>(
    tool_name: &str,
    tool_arguments: Map<String, Value>,
    call_timeout: Duration,
) -> Result<ToolResult, TatolabCommandFailure> {
    let tool_result_text =
        call_one_tool_of_the_running_runtime(tool_name, tool_arguments, call_timeout)?;
    serde_json::from_str(&tool_result_text).map_err(|parse_failure| {
        TatolabCommandFailure::refused(format!(
            "{tool_name} answered something other than its result ({parse_failure}): \
             {tool_result_text}"
        ))
    })
}

fn listed_relays() -> Result<Vec<ListedRelayInstance>, TatolabCommandFailure> {
    let list_relays_result: ListRelaysToolResult = call_one_relay_tool(
        "list_relays",
        Map::new(),
        OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
    )?;
    Ok(list_relays_result.relays)
}

/// `tatolab relays`.
pub(crate) fn list_relays() -> Result<u8, TatolabCommandFailure> {
    write_verb_standard_output(&rendered_relays_table(&listed_relays()?))
}

/// `tatolab relay add ADAPTER [--settings-file PATH]`.
fn add_relay(adapter: &str, settings_file: Option<&Path>) -> Result<u8, TatolabCommandFailure> {
    let settings = match settings_file {
        Some(settings_file) => settings_read_from(settings_file)?,
        None => Map::new(),
    };
    let mut add_relay_arguments = Map::new();
    add_relay_arguments.insert("adapter".to_owned(), adapter.into());
    add_relay_arguments.insert("settings".to_owned(), Value::Object(settings));
    let added: AddRelayToolResult = call_one_relay_tool(
        "add_relay",
        add_relay_arguments,
        RELAY_ACTION_TOOL_CALL_TIMEOUT,
    )?;
    write_verb_standard_output(&format!("{} added (adapter {adapter})\n", added.relay_id))
}

fn settings_read_from(settings_file: &Path) -> Result<Map<String, Value>, TatolabCommandFailure> {
    let settings_text = std::fs::read_to_string(settings_file).map_err(|not_read| {
        TatolabCommandFailure::refused(format!(
            "the settings file {} cannot be read: {not_read}",
            settings_file.display()
        ))
    })?;
    match serde_json::from_str(&settings_text) {
        Ok(Value::Object(settings)) => Ok(settings),
        _ => Err(TatolabCommandFailure::refused(format!(
            "the settings file {} is not a JSON object",
            settings_file.display()
        ))),
    }
}

/// `KEY=VALUE` as one setting, its value read as JSON when it parses and as text otherwise.
fn setting_from_key_and_value(
    key_and_value: &str,
) -> Result<(String, Value), TatolabCommandFailure> {
    let Some((key, value)) = key_and_value
        .split_once('=')
        .filter(|(key, _)| !key.is_empty())
    else {
        return Err(TatolabCommandFailure::refused(format!(
            "`{key_and_value}` is not KEY=VALUE"
        )));
    };
    let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()));
    Ok((key.to_owned(), value))
}

/// `tatolab relay set RELAY_ID [KEY=VALUE…] [--secret FIELD (--from-file PATH | --from-stdin)]`.
fn configure_relay(
    relay_id: &str,
    key_and_value_settings: &[String],
    secret: Option<(String, SecretSource)>,
) -> Result<u8, TatolabCommandFailure> {
    if key_and_value_settings.is_empty() && secret.is_none() {
        return Err(TatolabCommandFailure::refused(
            "name a setting as KEY=VALUE, or a secret with --secret FIELD".to_owned(),
        ));
    }
    let mut settings = Map::new();
    for key_and_value in key_and_value_settings {
        let (key, value) = setting_from_key_and_value(key_and_value)?;
        settings.insert(key, value);
    }
    if let Some(listed) = listed_relays()?
        .into_iter()
        .find(|listed| listed.relay_id == relay_id)
    {
        refuse_a_secret_as_an_argument_or_a_setting_as_a_secret(&listed, &settings, &secret)?;
    }
    if let Some((field, secret_source)) = &secret {
        settings.insert(
            field.clone(),
            Value::String(secret_source.read_the_secret()?),
        );
    }
    let mut configure_relay_arguments = Map::new();
    configure_relay_arguments.insert("relay_id".to_owned(), relay_id.into());
    configure_relay_arguments.insert("settings".to_owned(), Value::Object(settings));
    let configured: ConfigureRelayToolResult = call_one_relay_tool(
        "configure_relay",
        configure_relay_arguments,
        RELAY_ACTION_TOOL_CALL_TIMEOUT,
    )?;
    write_verb_standard_output(&format!("{} configured\n", configured.relay_id))
}

fn refuse_a_secret_as_an_argument_or_a_setting_as_a_secret(
    listed: &ListedRelayInstance,
    settings: &Map<String, Value>,
    secret: &Option<(String, SecretSource)>,
) -> Result<(), TatolabCommandFailure> {
    let relay_id = &listed.relay_id;
    if let Some(secret_setting) = settings
        .keys()
        .find(|key| listed.secrets.contains_key(*key))
    {
        return Err(TatolabCommandFailure::refused(format!(
            "`{secret_setting}` is a secret of the relay `{relay_id}`, and a secret is never an \
             argument: `tatolab relay set {relay_id} --secret {secret_setting} --from-file \
             <path>` or `--from-stdin`"
        )));
    }
    if let Some((field, _)) = secret
        && !listed.secrets.contains_key(field)
    {
        return Err(TatolabCommandFailure::refused(format!(
            "`{field}` is not a secret of the relay `{relay_id}`; its secrets are {}",
            if listed.secrets.is_empty() {
                "none".to_owned()
            } else {
                listed
                    .secrets
                    .keys()
                    .map(|secret_setting| format!("`{secret_setting}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )));
    }
    Ok(())
}

/// `tatolab relay rm RELAY_ID`.
fn remove_relay(relay_id: &str) -> Result<u8, TatolabCommandFailure> {
    let mut remove_relay_arguments = Map::new();
    remove_relay_arguments.insert("relay_id".to_owned(), relay_id.into());
    let removed: RemoveRelayToolResult = call_one_relay_tool(
        "remove_relay",
        remove_relay_arguments,
        RELAY_ACTION_TOOL_CALL_TIMEOUT,
    )?;
    write_verb_standard_output(&format!("{} removed\n", removed.relay_id))
}

/// The `RELAY  ADAPTER  STATE  SETTINGS  SECRETS` table, each column as wide as its widest cell,
/// then a line per relay not joined saying why; one line when there is no relay.
fn rendered_relays_table(listed_relays: &[ListedRelayInstance]) -> String {
    if listed_relays.is_empty() {
        return NO_RELAYS_IN_THIS_RUNTIME_LINE.to_owned();
    }
    let header_row = ["RELAY", "ADAPTER", "STATE", "SETTINGS", "SECRETS"].map(str::to_owned);
    let relay_rows = listed_relays.iter().map(|listed| {
        [
            if listed.built_in {
                format!("{} (built in)", listed.relay_id)
            } else {
                listed.relay_id.clone()
            },
            listed.adapter.clone(),
            match &listed.connection_state {
                ListedRelayConnectionState::NotJoined { .. } => "not joined".to_owned(),
                ListedRelayConnectionState::Lost { .. } => "lost".to_owned(),
                joining_or_joined => joining_or_joined.to_string(),
            },
            Value::Object(listed.settings.clone()).to_string(),
            listed
                .secrets
                .iter()
                .map(|(secret_setting, presence)| format!("{secret_setting} {presence}"))
                .collect::<Vec<_>>()
                .join(", "),
        ]
    });
    let table_rows: Vec<[String; 5]> = std::iter::once(header_row).chain(relay_rows).collect();
    let column_width = |column_index: usize| {
        table_rows
            .iter()
            .map(|table_row| table_row[column_index].chars().count())
            .max()
            .unwrap_or_default()
    };
    let [relay_width, adapter_width, state_width, settings_width] = [
        column_width(0),
        column_width(1),
        column_width(2),
        column_width(3),
    ];
    let mut rendered_table: String = table_rows
        .iter()
        .map(|[relay, adapter, state, settings, secrets]| {
            format!(
                "{relay:<relay_width$}  {adapter:<adapter_width$}  {state:<state_width$}  \
                 {settings:<settings_width$}  {secrets}"
            )
            .trim_end()
            .to_owned()
                + "\n"
        })
        .collect();
    let why_each_is_not_joined: Vec<String> = listed_relays
        .iter()
        .filter_map(|listed| match &listed.connection_state {
            ListedRelayConnectionState::NotJoined { why: Some(_) }
            | ListedRelayConnectionState::Lost { .. } => Some(format!(
                "{} is {}\n",
                listed.relay_id, listed.connection_state
            )),
            _ => None,
        })
        .collect();
    if !why_each_is_not_joined.is_empty() {
        rendered_table.push('\n');
        rendered_table.extend(why_each_is_not_joined);
    }
    rendered_table
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_runtime_client_contract::local_api_wire_contract::ListedRelaySecretPresence;

    fn a_listed_relay(
        relay_id: &str,
        secrets: &[(&str, ListedRelaySecretPresence)],
        connection_state: ListedRelayConnectionState,
    ) -> ListedRelayInstance {
        ListedRelayInstance {
            relay_id: relay_id.to_owned(),
            adapter: "provider".to_owned(),
            built_in: false,
            settings: serde_json::json!({"region": "us"})
                .as_object()
                .unwrap()
                .clone(),
            secrets: secrets
                .iter()
                .map(|(secret_setting, presence)| ((*secret_setting).to_owned(), *presence))
                .collect(),
            connection_state,
        }
    }

    #[test]
    fn a_value_reads_as_json_when_it_parses_and_as_text_otherwise() {
        for (key_and_value, expected) in [
            ("port=4500", ("port", serde_json::json!(4500))),
            ("region=us-east", ("region", serde_json::json!("us-east"))),
            ("tls=true", ("tls", serde_json::json!(true))),
            ("port=null", ("port", Value::Null)),
            ("note=a=b", ("note", serde_json::json!("a=b"))),
        ] {
            let (key, value) = setting_from_key_and_value(key_and_value).unwrap();
            assert_eq!((key.as_str(), value), (expected.0, expected.1));
        }
        let refusal = TatolabCommandFailure::refusal_message_of(setting_from_key_and_value("=4"));
        assert!(refusal.contains("KEY=VALUE"), "{refusal}");
    }

    #[test]
    fn a_secret_given_as_an_argument_is_refused_by_name_pointing_at_the_file_and_stdin_forms() {
        let listed = a_listed_relay(
            "provider-1",
            &[("token", ListedRelaySecretPresence::Set)],
            ListedRelayConnectionState::Joined,
        );
        let settings = serde_json::json!({"token": "s3cr3t"})
            .as_object()
            .unwrap()
            .clone();
        let refusal = TatolabCommandFailure::refusal_message_of(
            refuse_a_secret_as_an_argument_or_a_setting_as_a_secret(&listed, &settings, &None),
        );
        assert_eq!(
            refusal,
            "`token` is a secret of the relay `provider-1`, and a secret is never an argument: \
             `tatolab relay set provider-1 --secret token --from-file <path>` or `--from-stdin`"
        );
        assert!(!refusal.contains("s3cr3t"));
    }

    #[test]
    fn a_setting_named_as_a_secret_is_refused_naming_the_secrets() {
        let listed = a_listed_relay(
            "provider-1",
            &[("token", ListedRelaySecretPresence::Unset)],
            ListedRelayConnectionState::Joined,
        );
        let refusal = TatolabCommandFailure::refusal_message_of(
            refuse_a_secret_as_an_argument_or_a_setting_as_a_secret(
                &listed,
                &Map::new(),
                &Some(("region".to_owned(), SecretSource::StandardInput)),
            ),
        );
        assert_eq!(
            refusal,
            "`region` is not a secret of the relay `provider-1`; its secrets are `token`"
        );
    }

    #[test]
    fn the_table_shows_each_secret_set_or_unset_and_says_why_a_relay_is_not_joined() {
        let mut built_in = a_listed_relay(
            "local-loopback",
            &[],
            ListedRelayConnectionState::NotJoined {
                why: Some("no moq-relay".to_owned()),
            },
        );
        built_in.adapter = "local".to_owned();
        built_in.built_in = true;
        built_in.settings = Map::new();
        let rendered = rendered_relays_table(&[
            built_in,
            a_listed_relay(
                "provider-1",
                &[("token", ListedRelaySecretPresence::Set)],
                ListedRelayConnectionState::Joined,
            ),
        ]);
        assert_eq!(
            rendered,
            "RELAY                      ADAPTER   STATE       SETTINGS         SECRETS\n\
             local-loopback (built in)  local     not joined  {}\n\
             provider-1                 provider  joined      {\"region\":\"us\"}  token set\n\
             \n\
             local-loopback is not joined: no moq-relay\n"
        );
        assert_eq!(rendered_relays_table(&[]), "no relays\n");
    }
}
