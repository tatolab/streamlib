// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The classes `tatolab.moq` starts each node's native half with.

use std::time::Duration;

use pyo3::buffer::PyBuffer;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostInputPort, ModuleHostOutputPort, packets_unit_payload_descriptor,
    records_message_payload_descriptor,
};

use crate::error::MoqModuleError;
use crate::moq_gateway::MoqGatewayThread as MoqGatewayRunningThread;
use crate::moq_gateway_relay_settings::MoqGatewayRelaySettings;
use crate::moq_node_ipc::{MoqNodeIpc, MoqStreamServiceNames};
use crate::moq_track_receive::{
    MoqReceivedTrackFamily, MoqReceivedTrackOutput, MoqTrackReceiveAsked,
    MoqTrackReceiveThread as MoqTrackReceiveRunningThread,
};
use crate::moq_track_send::{
    MoqTrackDeclarationThread as MoqTrackDeclarationRunningThread, MoqTrackDeclared,
    MoqTrackSendPublisher as MoqTrackSendRunningPublisher,
};

/// The port a gateway reads its relay settings on.
const GATEWAY_SETTINGS_INPUT_PORT: &str = "settings";

/// The port a receive node writes its track on.
const RECEIVE_CONTENT_OUTPUT_PORT: &str = "content";

fn the_ipc_of_the_stream(runtime_id: &str, stream_id: &str) -> PyResult<MoqNodeIpc> {
    Ok(MoqNodeIpc::open_under_the_runtimes_domain_root(
        MoqStreamServiceNames::of_the_stream(runtime_id, stream_id),
    )?)
}

fn json_of(python_value: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    let json_text: String = python_value
        .py()
        .import("json")?
        .call_method1("dumps", (python_value,))?
        .extract()?;
    serde_json::from_str(&json_text).map_err(|refusal| {
        MoqModuleError::Refused {
            what: format!("a value is not JSON: {refusal}"),
        }
        .into()
    })
}

/// A gateway's native half: the one relay session of its stream's MoQ nodes.
#[pyclass]
struct MoqGatewayThread {
    gateway: MoqGatewayRunningThread,
}

#[pymethods]
impl MoqGatewayThread {
    /// Start the gateway `gateway_node`, dialing `config_settings` and each
    /// set of settings `settings` receives after.
    #[new]
    #[pyo3(signature = (module_host_table, *, runtime_id, stream_id, gateway_node, config_settings))]
    fn start(
        module_host_table: &Bound<'_, PyAny>,
        runtime_id: &str,
        stream_id: &str,
        gateway_node: String,
        config_settings: &Bound<'_, PyDict>,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        forward_this_modules_tracing_records_to(&module_host);
        let settings_input = module_host.open_input_port(GATEWAY_SETTINGS_INPUT_PORT)?;
        let serde_json::Value::Object(config_settings) = json_of(config_settings.as_any())? else {
            return Err(MoqModuleError::Refused {
                what: "the gateway's settings are not an object".into(),
            }
            .into());
        };
        let ipc = the_ipc_of_the_stream(runtime_id, stream_id)?;
        let gateway = MoqGatewayRunningThread::spawn(
            ipc,
            gateway_node,
            the_settings_source_of(settings_input, config_settings),
        )?;
        Ok(Self { gateway })
    }

    /// Ask the thread to stop and wait up to `timeout_seconds` for it: `True`
    /// when it ended.
    fn stop_and_join(&mut self, python: Python<'_>, timeout_seconds: f64) -> bool {
        let timeout = Duration::try_from_secs_f64(timeout_seconds).unwrap_or(Duration::ZERO);
        python.detach(|| self.gateway.stop_and_join(timeout))
    }
}

/// The settings a gateway dials: its config's first, then its config with the
/// values of each Records message `settings` receives laid over it, a `null`
/// unsetting its key. A config that does not read alone, such as one leaving
/// its credential to `settings`, waits for the first message.
fn the_settings_source_of(
    settings_input: ModuleHostInputPort,
    config_settings: serde_json::Map<String, serde_json::Value>,
) -> crate::moq_gateway::MoqGatewaySettingsSource {
    let mut config_settings_not_yet_read = true;
    Box::new(move || {
        if std::mem::take(&mut config_settings_not_yet_read) {
            match MoqGatewayRelaySettings::read_from(&config_settings) {
                Ok(settings) => return Some(settings),
                Err(refusal) => tracing::info!(
                    "MoqGateway: the config alone does not dial ({refusal}); waiting for `settings`"
                ),
            }
        }
        let mut newest_message = None;
        loop {
            match settings_input.read() {
                Ok(Some(message)) => newest_message = Some(message),
                Ok(None) => break,
                Err(refusal) => {
                    tracing::warn!("MoqGateway: `settings` did not read: {refusal}");
                    break;
                }
            }
        }
        let message = newest_message?;
        let overrides = match the_json_map_of_a_records_message(message.payload()) {
            Ok(overrides) => overrides,
            Err(refusal) => {
                tracing::warn!("MoqGateway: a `settings` message is refused: {refusal}");
                return None;
            }
        };
        let mut settings = config_settings.clone();
        for (key, value) in overrides {
            if value.is_null() {
                settings.remove(&key);
            } else {
                settings.insert(key, value);
            }
        }
        match MoqGatewayRelaySettings::read_from(&settings) {
            Ok(settings) => Some(settings),
            Err(refusal) => {
                tracing::warn!("MoqGateway: a `settings` message is refused: {refusal}");
                None
            }
        }
    })
}

/// A Records message's msgpack map as JSON.
fn the_json_map_of_a_records_message(
    payload: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let value = rmpv::decode::read_value(&mut &payload[..])
        .map_err(|refusal| format!("it is not msgpack: {refusal}"))?;
    match json_of_msgpack(value)? {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err("it is not a map".into()),
    }
}

fn json_of_msgpack(value: rmpv::Value) -> Result<serde_json::Value, String> {
    use rmpv::Value as Msgpack;
    use serde_json::Value as Json;
    Ok(match value {
        Msgpack::Nil => Json::Null,
        Msgpack::Boolean(boolean) => Json::Bool(boolean),
        Msgpack::Integer(integer) => match (integer.as_i64(), integer.as_u64()) {
            (Some(signed), _) => Json::from(signed),
            (None, Some(unsigned)) => Json::from(unsigned),
            (None, None) => return Err("an integer does not fit 64 bits".into()),
        },
        Msgpack::F32(float) => Json::from(f64::from(float)),
        Msgpack::F64(float) => Json::from(float),
        Msgpack::String(text) => Json::String(
            text.into_str()
                .ok_or_else(|| "a string is not UTF-8".to_string())?,
        ),
        Msgpack::Binary(_) | Msgpack::Ext(..) => {
            return Err("binary values have no place in settings".into());
        }
        Msgpack::Array(items) => Json::Array(
            items
                .into_iter()
                .map(json_of_msgpack)
                .collect::<Result<_, _>>()?,
        ),
        Msgpack::Map(entries) => {
            let mut map = serde_json::Map::new();
            for (key, value) in entries {
                let Msgpack::String(key) = key else {
                    return Err("a map key is not a string".into());
                };
                let key = key
                    .into_str()
                    .ok_or_else(|| "a map key is not UTF-8".to_string())?;
                map.insert(key, json_of_msgpack(value)?);
            }
            Json::Object(map)
        }
    })
}

/// A track node's native half: its declaration on its gateway.
#[pyclass]
struct MoqTrackDeclarationThread {
    declaration: MoqTrackDeclarationRunningThread,
}

#[pymethods]
impl MoqTrackDeclarationThread {
    /// Declare `track` on `gateway_node` for `track_node`, again every second.
    #[new]
    #[pyo3(signature = (module_host_table, *, runtime_id, stream_id, track_node, gateway_node, track, catalog_entry=None))]
    fn start(
        module_host_table: &Bound<'_, PyAny>,
        runtime_id: &str,
        stream_id: &str,
        track_node: String,
        gateway_node: String,
        track: String,
        catalog_entry: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        forward_this_modules_tracing_records_to(
            &ModuleHost::from_python_module_host_table_capsule(module_host_table)?,
        );
        let catalog_entry = catalog_entry.map(json_of).transpose()?;
        let declaration = MoqTrackDeclarationRunningThread::spawn(
            the_ipc_of_the_stream(runtime_id, stream_id)?,
            MoqTrackDeclared {
                track_node,
                gateway_node,
                track,
                catalog_entry,
            },
        )?;
        Ok(Self { declaration })
    }

    /// Stop declaring the track.
    fn stop_and_join(&mut self, python: Python<'_>) {
        python.detach(|| self.declaration.stop_and_join());
    }
}

/// A send node's native half: its writes to the track its config names.
#[pyclass]
struct MoqTrackSendPublisher {
    publisher: MoqTrackSendRunningPublisher,
}

#[pymethods]
impl MoqTrackSendPublisher {
    /// Write to the track `track_node` declares.
    #[new]
    #[pyo3(signature = (module_host_table, *, runtime_id, stream_id, track_node))]
    fn open(
        module_host_table: &Bound<'_, PyAny>,
        runtime_id: &str,
        stream_id: &str,
        track_node: &str,
    ) -> PyResult<Self> {
        forward_this_modules_tracing_records_to(
            &ModuleHost::from_python_module_host_table_capsule(module_host_table)?,
        );
        Ok(Self {
            publisher: MoqTrackSendRunningPublisher::open(
                the_ipc_of_the_stream(runtime_id, stream_id)?,
                track_node,
            )?,
        })
    }

    /// Hand the gateway one message, its payload any contiguous byte buffer.
    fn send_message(
        &mut self,
        payload: PyBuffer<u8>,
        timestamp_ns: i64,
        starts_group: bool,
        lost_since_last_read: u64,
    ) -> PyResult<()> {
        if !payload.is_c_contiguous() {
            return Err(MoqModuleError::Refused {
                what: "MoqTrackSend sends a contiguous payload only".into(),
            }
            .into());
        }
        // SAFETY: the buffer is C-contiguous, `item_count` bytes long, and held
        // exported by `payload` for the length of this call.
        let payload_bytes = unsafe {
            std::slice::from_raw_parts(payload.buf_ptr().cast::<u8>(), payload.item_count())
        };
        let sent = self.publisher.send_message(
            payload_bytes,
            timestamp_ns,
            starts_group,
            lost_since_last_read,
        );
        Ok(sent?)
    }

    /// Hand the gateway the hang entry the stream facts of the port feeding
    /// the send build, in their wire form, when it changed.
    #[pyo3(signature = (upstream_stream_facts=None))]
    fn send_the_upstream_stream_facts_when_they_changed(
        &mut self,
        upstream_stream_facts: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let upstream_stream_facts = upstream_stream_facts.map(json_of).transpose()?;
        Ok(self
            .publisher
            .send_the_upstream_stream_facts_when_they_changed(upstream_stream_facts)?)
    }
}

/// A receive node's `content` output, lent to its native half.
struct MoqReceivedTrackModuleHostOutput {
    module_host: ModuleHost,
    content: ModuleHostOutputPort,
    family: MoqReceivedTrackFamily,
}

impl MoqReceivedTrackOutput for MoqReceivedTrackModuleHostOutput {
    fn write_object(&mut self, payload: &[u8], timestamp_ns: i64, starts_group: bool) -> bool {
        let payload_descriptor = match &self.family {
            MoqReceivedTrackFamily::Records => Ok(records_message_payload_descriptor()),
            MoqReceivedTrackFamily::Packets { codec } => {
                packets_unit_payload_descriptor(codec).map_err(|refusal| format!("{refusal:?}"))
            }
        };
        let written = payload_descriptor.and_then(|payload_descriptor| {
            self.content
                .write_described(
                    payload,
                    Some(timestamp_ns),
                    starts_group,
                    &payload_descriptor,
                )
                .map_err(|refusal| refusal.to_string())
        });
        match written {
            Ok(()) => true,
            Err(refusal) => {
                tracing::debug!("MoqTrackReceive: an object was not written: {refusal}");
                false
            }
        }
    }

    fn declare_groups_given_up(&mut self, given_up_group_count: u64) {
        if let Err(refusal) = self
            .content
            .declare_groups_given_up_before_the_next_write(given_up_group_count)
        {
            tracing::warn!("MoqTrackReceive: given-up groups reach no reader: {refusal}");
        }
    }

    fn set_packets_stream_facts_read_from_the_remote_catalog_entry(
        &mut self,
        packets_stream_facts: &serde_json::Value,
    ) {
        if !matches!(self.family, MoqReceivedTrackFamily::Packets { .. }) {
            return;
        }
        if let Err(refusal) = self
            .content
            .set_stream_facts("packets", &packets_stream_facts.to_string())
        {
            tracing::warn!("MoqTrackReceive: the stream facts are not carried: {refusal}");
        }
    }

    fn stream_time_now_ns(&self) -> i64 {
        self.module_host
            .stream_time_ns_of_a_machine_instant(self.module_host.monotonic_now_ns())
    }
}

/// A receive node's native half: it asks its gateway for a track and writes
/// what arrives onto `content`.
#[pyclass]
struct MoqTrackReceiveThread {
    receive: MoqTrackReceiveRunningThread,
}

#[pymethods]
impl MoqTrackReceiveThread {
    /// Ask `gateway_node` for `track` of `broadcast` and write it onto `content`.
    #[new]
    #[pyo3(signature = (
        module_host_table, *, runtime_id, stream_id, receive_node, gateway_node, broadcast,
        track, max_age_milliseconds, family, codec=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn start(
        module_host_table: &Bound<'_, PyAny>,
        runtime_id: &str,
        stream_id: &str,
        receive_node: String,
        gateway_node: String,
        broadcast: String,
        track: String,
        max_age_milliseconds: u64,
        family: &str,
        codec: Option<String>,
    ) -> PyResult<Self> {
        let family = match family {
            "packets" => MoqReceivedTrackFamily::Packets {
                codec: codec.unwrap_or_default(),
            },
            "records" => MoqReceivedTrackFamily::Records,
            other => {
                return Err(MoqModuleError::Refused {
                    what: format!(
                        "MoqTrackReceive carries Packets or Records, and `family` names {other:?}"
                    ),
                }
                .into());
            }
        };
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        forward_this_modules_tracing_records_to(&module_host);
        let content = module_host.open_output_port(RECEIVE_CONTENT_OUTPUT_PORT)?;
        let receive = MoqTrackReceiveRunningThread::spawn(
            the_ipc_of_the_stream(runtime_id, stream_id)?,
            MoqTrackReceiveAsked {
                receive_node,
                gateway_node,
                broadcast,
                track,
                max_age: Duration::from_millis(max_age_milliseconds),
            },
            Box::new(MoqReceivedTrackModuleHostOutput {
                module_host,
                content,
                family,
            }),
        )?;
        Ok(Self { receive })
    }

    /// Ask the thread to stop and wait up to `timeout_seconds` for it: `True`
    /// when it ended.
    fn stop_and_join(&mut self, python: Python<'_>, timeout_seconds: f64) -> bool {
        let timeout = Duration::try_from_secs_f64(timeout_seconds).unwrap_or(Duration::ZERO);
        python.detach(|| self.receive.stop_and_join(timeout))
    }
}

/// The classes the Python half starts its nodes' native halves with.
pub fn add_the_node_classes_to(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<MoqGatewayThread>()?;
    module.add_class::<MoqTrackDeclarationThread>()?;
    module.add_class::<MoqTrackSendPublisher>()?;
    module.add_class::<MoqTrackReceiveThread>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settings_message_reads_as_its_json_map() {
        let mut payload = Vec::new();
        rmpv::encode::write_value(
            &mut payload,
            &rmpv::Value::Map(vec![
                ("broadcast".into(), "studio".into()),
                ("token".into(), rmpv::Value::Nil),
                ("port".into(), 4443.into()),
            ]),
        )
        .unwrap();
        assert_eq!(
            serde_json::Value::Object(the_json_map_of_a_records_message(&payload).unwrap()),
            serde_json::json!({"broadcast": "studio", "token": null, "port": 4443})
        );
        assert!(the_json_map_of_a_records_message(&[0x01]).is_err());
    }
}
