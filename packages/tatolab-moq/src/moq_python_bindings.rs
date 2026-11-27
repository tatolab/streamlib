// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The classes `tatolab.moq` starts each node's native half with.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};
use std::time::Duration;

use pyo3::buffer::PyBuffer;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostCallTooSlowForAFrame, ModuleHostInputPort, ModuleHostOutputPort,
    ModuleHostTablePayloadDescriptor, packets_unit_payload_descriptor,
};

use crate::error::MoqModuleError;
use crate::moq_gateway::MoqGatewayThread as MoqGatewayRunningThread;
use crate::moq_gateway_relay_settings::MoqGatewayRelaySettings;
use crate::moq_node_ipc::{MoqNodeIpc, MoqStreamServiceNames};
use crate::moq_track_receive::{
    MoqReceivedTrackContent, MoqReceivedTrackOutput, MoqTrackReceiveAsked,
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

/// How long `close()` waits for a node's native thread. Each thread's own wait
/// is bounded well under it; a relay session closing takes the longest.
const A_NATIVE_THREAD_ENDS_WITHIN: Duration = Duration::from_secs(5);

/// Wait, without the GIL, up to `A_NATIVE_THREAD_ENDS_WITHIN` for a node's native thread to
/// stop, warning with `still_finishing` when it has not.
fn close_waiting_for(
    python: Python<'_>,
    still_finishing: &str,
    stop_and_join: impl FnOnce(Duration) -> bool + Send,
) {
    if !python.detach(|| stop_and_join(A_NATIVE_THREAD_ENDS_WITHIN)) {
        tracing::warn!("{still_finishing}");
    }
}

/// A gateway's `settings` input, opened once by its node and read by each
/// gateway it starts: the settings the last message laid over the config
/// outlive the gateway that read them, so a gateway started for a new relay
/// address dials with the credential handed over before it.
#[pyclass(frozen)]
struct MoqGatewaySettingsInput {
    read: Arc<Mutex<MoqGatewaySettingsRead>>,
}

/// The settings input and the values its newest message set.
struct MoqGatewaySettingsRead {
    settings_input: ModuleHostInputPort,
    values_the_last_message_set: serde_json::Map<String, serde_json::Value>,
}

#[pymethods]
impl MoqGatewaySettingsInput {
    /// Open the `settings` input the gateway's module host table lends.
    #[new]
    fn open(module_host_table: &Bound<'_, PyAny>) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        Ok(Self {
            read: Arc::new(Mutex::new(MoqGatewaySettingsRead {
                settings_input: module_host.open_input_port(GATEWAY_SETTINGS_INPUT_PORT)?,
                values_the_last_message_set: serde_json::Map::new(),
            })),
        })
    }
}

/// A gateway's native half: the one relay session of its stream's MoQ nodes.
#[pyclass]
struct MoqGatewayThread {
    gateway: MoqGatewayRunningThread,
}

#[pymethods]
impl MoqGatewayThread {
    /// Start the gateway `gateway_node`, dialing `config_settings` with what
    /// `settings_input` received laid over them, and each set of settings it
    /// receives after. A gateway of the same node started before is closed
    /// first.
    #[new]
    #[pyo3(signature = (module_host_table, settings_input, *, runtime_id, stream_id, gateway_node, config_settings))]
    fn start(
        python: Python<'_>,
        module_host_table: &Bound<'_, PyAny>,
        settings_input: &MoqGatewaySettingsInput,
        runtime_id: &str,
        stream_id: &str,
        gateway_node: String,
        config_settings: &Bound<'_, PyDict>,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        module_host.refuse_while_a_frame_runs(
            ModuleHostCallTooSlowForAFrame::StartsASession,
            "MoqGatewayThread",
        )?;
        forward_this_modules_tracing_records_to(&module_host);
        let serde_json::Value::Object(config_settings) = json_of(config_settings.as_any())? else {
            return Err(MoqModuleError::Refused {
                what: "the gateway's settings are not an object".into(),
            }
            .into());
        };
        let ipc = the_ipc_of_the_stream(runtime_id, stream_id)?;
        let settings_source =
            the_settings_source_of(Arc::clone(&settings_input.read), config_settings);
        let gateway =
            python.detach(|| MoqGatewayRunningThread::spawn(ipc, gateway_node, settings_source))?;
        Ok(Self { gateway })
    }

    /// Close the relay session and stop the thread, waiting for it; a gateway
    /// started after it for the same node has closed it already.
    fn close(&mut self, python: Python<'_>) {
        close_waiting_for(
            python,
            "MoqGateway: the relay session is still closing",
            |timeout| self.gateway.stop_and_join(timeout),
        );
    }
}

/// The settings a gateway dials: its config with the values the newest
/// `MoqRelaySettings` message `settings` received laid over it, a `null`
/// unsetting its key, first, then again each time a message arrives. A config
/// that does not read alone, such as one leaving its credential to `settings`,
/// waits for the first message.
fn the_settings_source_of(
    settings_read: Arc<Mutex<MoqGatewaySettingsRead>>,
    config_settings: serde_json::Map<String, serde_json::Value>,
) -> crate::moq_gateway::MoqGatewaySettingsSource {
    let mut settings_not_yet_read = true;
    Box::new(move || {
        let first_read = std::mem::take(&mut settings_not_yet_read);
        let mut settings_read = settings_read.lock().unwrap_or_else(PoisonError::into_inner);
        let mut newest_message = None;
        loop {
            match settings_read.settings_input.read() {
                Ok(Some(message)) => newest_message = Some(message),
                Ok(None) => break,
                Err(refusal) => {
                    tracing::warn!("MoqGateway: `settings` did not read: {refusal}");
                    break;
                }
            }
        }
        let values_changed = match newest_message
            .map(|message| the_json_map_of_a_settings_message(message.payload()))
        {
            Some(Ok(values)) => {
                settings_read.values_the_last_message_set = values;
                true
            }
            Some(Err(refusal)) => {
                tracing::warn!("MoqGateway: a `settings` message is refused: {refusal}");
                false
            }
            None => false,
        };
        if !values_changed && !first_read {
            return None;
        }
        let settings = the_config_with_the_message_values_laid_over(
            &config_settings,
            &settings_read.values_the_last_message_set,
        );
        match MoqGatewayRelaySettings::read_from(&settings) {
            Ok(settings) => Some(settings),
            Err(refusal) => {
                tracing::info!(
                    "MoqGateway: the settings do not dial ({refusal}); waiting for `settings`"
                );
                None
            }
        }
    })
}

/// `config_settings` with each of `message_values` laid over it, a `null` unsetting its key.
fn the_config_with_the_message_values_laid_over(
    config_settings: &serde_json::Map<String, serde_json::Value>,
    message_values: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut settings = config_settings.clone();
    for (key, value) in message_values {
        if value.is_null() {
            settings.remove(key);
        } else {
            settings.insert(key.clone(), value.clone());
        }
    }
    settings
}

/// The JSON map a `MoqRelaySettings` message packs.
fn the_json_map_of_a_settings_message(
    payload: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    serde_json::from_slice(payload).map_err(|refusal| format!("it is not a JSON map: {refusal}"))
}

/// A track node's native half: its declaration on its gateway.
#[pyclass]
struct MoqTrackDeclarationThread {
    declaration: MoqTrackDeclarationRunningThread,
}

#[pymethods]
impl MoqTrackDeclarationThread {
    /// Declare `track` on `gateway_node` for `track_node`, again every second.
    /// A declaration of the same track node started before is closed first.
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
        let ipc = the_ipc_of_the_stream(runtime_id, stream_id)?;
        let declared = MoqTrackDeclared {
            track_node,
            gateway_node,
            track,
            catalog_entry,
        };
        let declaration = module_host_table
            .py()
            .detach(|| MoqTrackDeclarationRunningThread::spawn(ipc, declared))?;
        Ok(Self { declaration })
    }

    /// Stop declaring the track and wait for the thread.
    fn close(&mut self, python: Python<'_>) {
        close_waiting_for(
            python,
            "MoqTrack: the declaration thread is still finishing",
            |timeout| self.declaration.stop_and_join(timeout),
        );
    }
}

/// A send's publisher, gone once closed or taken over.
type HeldPublisher = Mutex<Option<MoqTrackSendRunningPublisher>>;

/// The publisher writing to each track node in this process, by the name of its content
/// service: a send's description and content services admit one holder.
static THE_PUBLISHER_OF_EACH_TRACK_NODE: LazyLock<Mutex<HashMap<String, Weak<HeldPublisher>>>> =
    LazyLock::new(Mutex::default);

/// A publisher writing to the track `track_node` declares, as the one publisher of this process
/// writing to it: one opened before, by a send rewired away and back before its value was
/// closed, is closed first.
fn the_publisher_taking_over(
    ipc: MoqNodeIpc,
    track_node: &str,
) -> Result<Arc<HeldPublisher>, MoqModuleError> {
    let mut publishers = THE_PUBLISHER_OF_EACH_TRACK_NODE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    publishers.retain(|_, held| held.strong_count() > 0);
    let content_service = ipc.service_names.track_content(track_node);
    if let Some(held_before) = publishers.get(&content_service).and_then(Weak::upgrade) {
        *held_before.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    let held = Arc::new(Mutex::new(Some(MoqTrackSendRunningPublisher::open(
        ipc, track_node,
    )?)));
    publishers.insert(content_service, Arc::downgrade(&held));
    Ok(held)
}

/// A send node's native half: its writes to the track its config names.
#[pyclass]
struct MoqTrackSendPublisher {
    publisher: Arc<HeldPublisher>,
}

impl MoqTrackSendPublisher {
    fn with_the_publisher<T>(
        &self,
        write: impl FnOnce(&mut MoqTrackSendRunningPublisher) -> crate::error::Result<T>,
    ) -> PyResult<T> {
        let mut publisher = self
            .publisher
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let publisher = publisher.as_mut().ok_or(MoqModuleError::Closed {
            what: "MoqTrackSend's publisher",
        })?;
        Ok(write(publisher)?)
    }
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
            publisher: the_publisher_taking_over(
                the_ipc_of_the_stream(runtime_id, stream_id)?,
                track_node,
            )?,
        })
    }

    /// Give up the track's content service, which a publisher for another
    /// track the send is wired leaves to it.
    fn close(&mut self) {
        *self
            .publisher
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
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
        self.with_the_publisher(|publisher| {
            publisher.send_message(
                payload_bytes,
                timestamp_ns,
                starts_group,
                lost_since_last_read,
            )
        })
    }

    /// Hand the gateway the hang entry the stream facts of the port feeding
    /// the send build, in their wire form, when it changed.
    #[pyo3(signature = (upstream_stream_facts=None))]
    fn send_the_upstream_stream_facts_when_they_changed(
        &mut self,
        upstream_stream_facts: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let upstream_stream_facts = upstream_stream_facts.map(json_of).transpose()?;
        self.with_the_publisher(|publisher| {
            publisher.send_the_upstream_stream_facts_when_they_changed(upstream_stream_facts)
        })
    }
}

/// A receive node's `content` output, lent to its native half.
struct MoqReceivedTrackModuleHostOutput {
    module_host: ModuleHost,
    content: ModuleHostOutputPort,
    carries: MoqReceivedTrackContent,
}

impl MoqReceivedTrackOutput for MoqReceivedTrackModuleHostOutput {
    fn write_object(&mut self, payload: &[u8], timestamp_ns: i64, starts_group: bool) -> bool {
        let payload_descriptor = match &self.carries {
            MoqReceivedTrackContent::Named => Ok(ModuleHostTablePayloadDescriptor::bytes()),
            MoqReceivedTrackContent::Packets { codec } => {
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
        if !matches!(self.carries, MoqReceivedTrackContent::Packets { .. }) {
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
    /// Ask `gateway_node` for `track` of `broadcast` and write it onto
    /// `content`. A receive of the same node started before is closed first.
    #[new]
    #[pyo3(signature = (
        module_host_table, *, runtime_id, stream_id, receive_node, gateway_node, broadcast,
        track, max_age_milliseconds, carries, codec=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn start(
        python: Python<'_>,
        module_host_table: &Bound<'_, PyAny>,
        runtime_id: &str,
        stream_id: &str,
        receive_node: String,
        gateway_node: String,
        broadcast: String,
        track: String,
        max_age_milliseconds: u64,
        carries: &str,
        codec: Option<String>,
    ) -> PyResult<Self> {
        let carries = match carries {
            "encoded" => MoqReceivedTrackContent::Packets {
                codec: codec.unwrap_or_default(),
            },
            "named" => MoqReceivedTrackContent::Named,
            other => {
                return Err(MoqModuleError::Refused {
                    what: format!(
                        "MoqTrackReceive carries encoded media or a named type, and `carries` names {other:?}"
                    ),
                }
                .into());
            }
        };
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        forward_this_modules_tracing_records_to(&module_host);
        let content = module_host.open_output_port(RECEIVE_CONTENT_OUTPUT_PORT)?;
        let ipc = the_ipc_of_the_stream(runtime_id, stream_id)?;
        let asked = MoqTrackReceiveAsked {
            receive_node,
            gateway_node,
            broadcast,
            track,
            max_age: Duration::from_millis(max_age_milliseconds),
        };
        let output = Box::new(MoqReceivedTrackModuleHostOutput {
            module_host,
            content,
            carries,
        });
        let receive = python.detach(|| MoqTrackReceiveRunningThread::spawn(ipc, asked, output))?;
        Ok(Self { receive })
    }

    /// Stop the thread and wait for it; a receive started after it for the
    /// same node has stopped it already.
    fn close(&mut self, python: Python<'_>) {
        close_waiting_for(
            python,
            "MoqTrackReceive: the receive thread is still finishing",
            |timeout| self.receive.stop_and_join(timeout),
        );
    }
}

/// The classes the Python half starts its nodes' native halves with.
pub fn add_the_node_classes_to(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<MoqGatewaySettingsInput>()?;
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
    fn a_publisher_opened_again_for_its_track_node_takes_it_over_from_the_one_before() {
        let domain_root = tempfile::Builder::new()
            .prefix("moq")
            .tempdir_in("/tmp")
            .unwrap();
        let ipc = MoqNodeIpc::open_under_the_domain_root(
            domain_root.path(),
            MoqStreamServiceNames::of_the_stream("run", "send-rewired-back"),
        )
        .expect("the iceoryx2 node opens");
        let first = the_publisher_taking_over(ipc.clone(), "telemetry").expect("the first opens");
        let again =
            the_publisher_taking_over(ipc, "telemetry").expect("the track node is taken over");
        assert!(first.lock().unwrap().is_none(), "the one before is closed");
        assert!(again.lock().unwrap().is_some());
    }

    #[test]
    fn a_settings_message_reads_as_its_json_map() {
        let payload = br#"{"broadcast": "studio", "token": null, "port": "4443"}"#;
        assert_eq!(
            serde_json::Value::Object(the_json_map_of_a_settings_message(payload).unwrap()),
            serde_json::json!({"broadcast": "studio", "token": null, "port": "4443"})
        );
        assert!(the_json_map_of_a_settings_message(b"[1]").is_err());
        assert!(the_json_map_of_a_settings_message(&[0x81]).is_err());
    }

    #[test]
    fn a_settings_message_lays_its_values_over_the_config_a_null_unsetting_one() {
        let config =
            serde_json::json!({"kind": "moq-relay", "relay_url": "https://a", "token": "t"});
        let message = serde_json::json!({"relay_url": "https://b", "token": null});
        assert_eq!(
            serde_json::Value::Object(the_config_with_the_message_values_laid_over(
                config.as_object().unwrap(),
                message.as_object().unwrap(),
            )),
            serde_json::json!({"kind": "moq-relay", "relay_url": "https://b"})
        );
    }
}
