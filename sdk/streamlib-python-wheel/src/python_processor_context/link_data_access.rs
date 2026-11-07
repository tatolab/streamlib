// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyString};
use streamlib::sdk::iceoryx2::{
    ChannelsBlockSampleType, DataChannelPayloadDescriptor, PacketsUnitDescriptorBody,
};

use crate::python_bag_conversion::records_message_payload_of;
use crate::python_port_payload::PythonPortPayloadLoan;
use crate::python_processor_link_data_access::{
    ArrayOutputPortDeclaredResidency, ChannelsOutputPortDeclaredShape, PortFamiliesOfTheRead,
    PortFamilyOfTheRead, PortPayloadReadShape, PythonProcessorLinkDataAccess,
};

use super::gpu_context::PythonGpuContextLimitedAccess;

/// A processor's input ports, as `ctx.inputs`.
///
/// It carries the same GPU capability the context exposes as
/// `ctx.gpu_limited_access` because this is where the two knowledges meet: the
/// consumer names the type it is reading into, and the context holds the route
/// to the engine's surfaces.
#[pyclass(name = "LinkInputDataReader", module = "tatolab.stream", frozen)]
pub(crate) struct PythonLinkInputDataReader {
    pub(super) link_data_access: Py<PythonProcessorLinkDataAccess>,
    pub(super) gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    /// The bridge's blocking round trip to the parent, which holds the
    /// catalog entries a read asks after.
    pub(super) escalate_request_to_parent: Option<Py<PyAny>>,
}

#[pymethods]
impl PythonLinkInputDataReader {
    /// The next message on `port`, a port name or an `Input` attribute, or
    /// `None` when the mailbox is empty: by name a `PortPayloadBorrow` of the
    /// sample it arrived in, through an attribute the port family's type.
    ///
    /// With `into`, the read returns `into.from_bytes(payload)`, so a payload
    /// that does not fit raises here rather than travelling on. `from_bytes` is
    /// offered this processor's GPU capability while it runs — see
    /// `gpu_limited_access_of_the_typed_read_in_progress`.
    #[pyo3(signature = (port, *, into = None))]
    fn read<'py>(
        &self,
        python: Python<'py>,
        port: &Bound<'py, PyAny>,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let (port_name, read_shape) = input_port_and_read_shape_named_by(port, into)?;
        self.link_data_access
            .get()
            .read_from_input_port_offering_gpu_access(
                python,
                &port_name,
                read_shape,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message's payload with its stamp, or `(None, None)` when empty.
    fn read_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port: &Bound<'py, PyAny>,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        let (port_name, read_shape) = input_port_and_read_shape_named_by(port, None)?;
        self.link_data_access
            .get()
            .read_from_input_port_with_timestamp_offering_gpu_access(
                python,
                &port_name,
                read_shape,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message's payload on `port` with the link it arrived on,
    /// or `None`.
    ///
    /// Any number of links may enter one input port, and each is one producer.
    /// This is how a many-input processor tells them apart: the name is the
    /// source channel the link subscribed to, which the engine knows and a
    /// producer cannot misstate.
    #[pyo3(signature = (port, *, into = None))]
    fn read_from_inbound_link<'py>(
        &self,
        python: Python<'py>,
        port: &Bound<'py, PyAny>,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String)>> {
        let (port_name, read_shape) = input_port_and_read_shape_named_by(port, into)?;
        self.link_data_access
            .get()
            .read_from_input_port_naming_its_inbound_link(
                python,
                &port_name,
                read_shape,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message's payload on `port` with its link and its
    /// timestamp, or `None`.
    ///
    /// What a many-track sink needs to restate a producer's own timing: the
    /// link names the producer and the stamp is the one that producer wrote,
    /// which is the source frame's instant rather than the moment of the read.
    #[pyo3(signature = (port, *, into = None))]
    fn read_from_inbound_link_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port: &Bound<'py, PyAny>,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String, i64)>> {
        let (port_name, read_shape) = input_port_and_read_shape_named_by(port, into)?;
        self.link_data_access
            .get()
            .read_from_input_port_naming_its_inbound_link_and_timestamp(
                python,
                &port_name,
                read_shape,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message on `port` as a `Message` — payload, stamp, group
    /// flag, the messages its link lost since the previous read from it, and
    /// the link — or `None`.
    ///
    /// `into` casts the payload as it does for `read`.
    #[pyo3(signature = (port, *, into = None))]
    fn read_message<'py>(
        &self,
        python: Python<'py>,
        port: &Bound<'py, PyAny>,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let (port_name, read_shape) = input_port_and_read_shape_named_by(port, into)?;
        self.link_data_access.get().read_message_from_input_port(
            python,
            &port_name,
            read_shape,
            Some(self.gpu_limited_access_context.bind(python)),
        )
    }

    /// Count the message last read from `port_name` as lost on the link that
    /// delivered it, once — for a message whose frame its producer recycled
    /// before this node's claim.
    fn count_the_message_last_read_as_lost(
        &self,
        python: Python<'_>,
        port_name: &str,
    ) -> PyResult<()> {
        self.link_data_access
            .get()
            .count_the_message_last_read_from_input_port_as_lost(python, port_name)
    }

    /// Every link feeding `port_name`, in wiring order.
    ///
    /// Readable in `setup()`, which is how a sink learns how many producers it
    /// owes before the first message arrives. A port nothing is connected to lists
    /// none.
    fn inbound_link_names(&self, port_name: &str) -> PyResult<Vec<String>> {
        self.link_data_access
            .get()
            .inbound_links_of_input_port(port_name)
    }

    /// Whether a message is waiting on `port_name`, without consuming it.
    fn has_data(&self, python: Python<'_>, port_name: &str) -> PyResult<bool> {
        self.link_data_access
            .get()
            .input_port_has_data(python, port_name)
    }

    /// The catalog entry the output port feeding `port_name` carries now, as
    /// `{"section": ..., "entry": {...}}`, or `None`.
    ///
    /// A port fed by more than one link names the one to read with
    /// `inbound_link`.
    #[pyo3(signature = (port_name, *, inbound_link = None))]
    fn inbound_link_catalog_entry<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        inbound_link: Option<&str>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let op = PyDict::new(python);
        op.set_item("op", "read_inbound_link_catalog_entry")?;
        op.set_item("port", port_name)?;
        if let Some(inbound_link) = inbound_link {
            op.set_item("inbound_link", inbound_link)?;
        }
        let response = catalog_entry_round_trip_to_parent(
            python,
            self.escalate_request_to_parent.as_ref(),
            &op,
        )?;
        let catalog_entry = response.call_method1("get", ("catalog_entry",))?;
        Ok((!catalog_entry.is_none()).then_some(catalog_entry))
    }
}

/// The input port `port` names, a name or an `Input` attribute, and what its
/// read hands back: through an attribute, the family's type; by name, the raw
/// borrow or what `into` casts it to.
fn input_port_and_read_shape_named_by<'target, 'py>(
    port: &Bound<'py, PyAny>,
    into: Option<&'target Bound<'py, PyAny>>,
) -> PyResult<(String, PortPayloadReadShape<'target, 'py>)> {
    if port.is_instance_of::<PyString>() {
        return Ok((
            port.extract()?,
            PortPayloadReadShape::by_name_cast_into(into),
        ));
    }
    let port_name = port_attribute_name(port, "Input")?;
    let read_shape = match into {
        Some(into) => PortPayloadReadShape::CastInto(into),
        None => {
            let family_of = |port_family: Bound<'py, PyAny>| -> PyResult<PortFamilyOfTheRead> {
                let declaration_name: String =
                    port_family.getattr("declaration_name")?.extract()?;
                PortFamilyOfTheRead::of_declaration_name(&declaration_name).ok_or_else(|| {
                    PyTypeError::new_err(format!(
                        "input port {port_name:?} declares family {declaration_name:?}, which \
                         is none of Image, Array, Channels, Packets, Records"
                    ))
                })
            };
            let declared_family = family_of(port.getattr("family")?)?;
            let also_taken_families = port
                .getattr("also_takes")?
                .try_iter()?
                .map(|port_family| family_of(port_family?))
                .collect::<PyResult<Vec<_>>>()?;
            PortPayloadReadShape::FamilyType(
                PortFamiliesOfTheRead::of_the_declared_family_and_also(
                    declared_family,
                    also_taken_families,
                ),
            )
        }
    };
    Ok((port_name, read_shape))
}

/// The port name a `tatolab.stream` `Input` or `Output` attribute, or one of
/// a subclass, names.
fn port_attribute_name(port: &Bound<'_, PyAny>, expected_class: &str) -> PyResult<String> {
    let port_type = port.get_type();
    let mut is_the_expected_port_class = false;
    for class in port_type.mro().iter() {
        let class = class.cast_into::<pyo3::types::PyType>()?;
        let module: String = class.getattr("__module__")?.extract()?;
        if class.name()? == expected_class && module.starts_with("tatolab.stream") {
            is_the_expected_port_class = true;
            break;
        }
    }
    if !is_the_expected_port_class {
        return Err(PyTypeError::new_err(format!(
            "a port is named by a str or a tatolab.stream {expected_class} attribute; got a {}",
            port_type.name()?
        )));
    }
    port.getattr("port_name")?.extract()
}

/// A processor's output ports, as `ctx.outputs`.
#[pyclass(name = "LinkOutputDataWriter", module = "tatolab.stream", frozen)]
pub(crate) struct PythonLinkOutputDataWriter {
    pub(super) link_data_access: Py<PythonProcessorLinkDataAccess>,
    /// What names the descriptor of a pooled GPU frame a write carries.
    pub(super) gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    /// The bridge's blocking round trip to the parent, which holds the
    /// catalog entries this processor's ports carry.
    pub(super) escalate_request_to_parent: Option<Py<PyAny>>,
}

#[pymethods]
impl PythonLinkOutputDataWriter {
    /// Publish one message to every downstream link on `port`: by name a copy
    /// of `payload`, any contiguous buffer; through an `Output` attribute the
    /// port family's type.
    #[pyo3(signature = (
        port,
        payload,
        timestamp_ns = None,
        starts_group = true,
        *,
        start_sample = None,
    ))]
    fn write(
        &self,
        python: Python<'_>,
        port: &Bound<'_, PyAny>,
        payload: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
        starts_group: bool,
        start_sample: Option<u64>,
    ) -> PyResult<()> {
        if !port.is_instance_of::<PyString>() {
            return self.write_through_an_output_port_attribute(
                python,
                port,
                payload,
                timestamp_ns,
                starts_group,
                start_sample,
            );
        }
        let port_name: String = port.extract()?;
        if start_sample.is_some() {
            return Err(PyTypeError::new_err(format!(
                "start_sample= belongs to a write through a Channels Output attribute; write \
                 {port_name:?} by name with write_channels_block(port_name, samples, \
                 start_sample=…, sample_rate=…)"
            )));
        }
        self.link_data_access.get().write_described_by(
            python,
            &port_name,
            payload,
            timestamp_ns,
            starts_group,
            |_payload_bytes| Ok(DataChannelPayloadDescriptor::bytes()),
        )
    }

    /// Publish `samples`, a C-contiguous `[channel][sample]` float32 or
    /// float64 array, as one Channels block on `port_name`.
    #[pyo3(signature = (port_name, samples, *, start_sample, sample_rate, timestamp_ns = None))]
    fn write_channels_block(
        &self,
        python: Python<'_>,
        port_name: &str,
        samples: &Bound<'_, PyAny>,
        start_sample: u64,
        sample_rate: u32,
        timestamp_ns: Option<i64>,
    ) -> PyResult<()> {
        self.link_data_access
            .get()
            .write_channels_block_to_output_port(
                python,
                port_name,
                samples,
                start_sample,
                sample_rate,
                ChannelsOutputPortDeclaredShape::default(),
                timestamp_ns,
                true,
            )
    }

    /// Loan `payload_byte_len` bytes on `port_name` to fill in place, as
    /// `with ctx.outputs.loan(port, n) as out:`; the sample is published when
    /// the block exits cleanly and given back when it raises.
    #[pyo3(signature = (port_name, payload_byte_len, timestamp_ns = None, starts_group = true))]
    fn loan(
        &self,
        python: Python<'_>,
        port_name: &str,
        payload_byte_len: usize,
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> PyResult<PythonPortPayloadLoan> {
        self.link_data_access.get().loan_on_output_port(
            python,
            port_name,
            payload_byte_len,
            timestamp_ns,
            starts_group,
        )
    }

    /// Replace the catalog entry `port_name` carries with `entry`, one of
    /// `tatolab.stream.catalog`'s entries. The parent parses it with hang's
    /// catalog types and refuses one they do not read.
    fn set_catalog_entry(
        &self,
        python: Python<'_>,
        port_name: &str,
        entry: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let not_a_catalog_entry = |_| {
            PyTypeError::new_err(format!(
                "output port {port_name:?}: set_catalog_entry takes one of tatolab.stream.catalog's \
                 entries, got {}",
                entry
                    .get_type()
                    .name()
                    .map(|type_name| type_name.to_string())
                    .unwrap_or_default()
            ))
        };
        let section = entry.getattr("section").map_err(not_a_catalog_entry)?;
        let fields = entry.call_method0("to_dict").map_err(not_a_catalog_entry)?;
        let op = PyDict::new(python);
        op.set_item("op", "set_output_port_catalog_entry")?;
        op.set_item("port", port_name)?;
        op.set_item("section", section)?;
        op.set_item("entry", fields)?;
        catalog_entry_round_trip_to_parent(python, self.escalate_request_to_parent.as_ref(), &op)?;
        Ok(())
    }
}

/// One catalog-entry escalate round trip to the parent, whose refusal raises
/// as the bridge raises it.
fn catalog_entry_round_trip_to_parent<'py>(
    python: Python<'py>,
    escalate_request_to_parent: Option<&Py<PyAny>>,
    op: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    escalate_request_to_parent
        .ok_or_else(|| {
            PyRuntimeError::new_err(
                "a catalog entry lives on the parent's graph, and this context has no route to \
                 the parent",
            )
        })?
        .bind(python)
        .call1((op,))
}

/// The typed cast's claim, over a real link and a real surface-share service.
///
/// What is proven here is the seam, not a type: a message crosses a wired link,
/// the read builds a frame class **the wheel does not ship** through its
/// `from_bytes`, and that class pins its surface for exactly as long as it
/// lives. The target is a class nothing in the SDK ships, so the claim is the
/// seam's and no type's private handshake.
#[cfg(all(test, target_os = "linux"))]
mod typed_read_claim_tests;

/// The names an `Output` attribute states on its class; none for names it
/// reads from config, which only the parent resolves.
fn names_the_output_port_attribute_states(port: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    match port.getattr("names")? {
        names if names.is_instance_of::<pyo3::types::PyTuple>() => names.extract(),
        _ => Ok(Vec::new()),
    }
}

impl PythonLinkOutputDataWriter {
    /// Write `payload` as the family of the `Output` attribute `port` declares.
    fn write_through_an_output_port_attribute(
        &self,
        python: Python<'_>,
        port: &Bound<'_, PyAny>,
        payload: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
        starts_group: bool,
        start_sample: Option<u64>,
    ) -> PyResult<()> {
        let port_name = port_attribute_name(port, "Output")?;
        let family: String = port
            .getattr("family")?
            .getattr("declaration_name")?
            .extract()?;
        let link_data_access = self.link_data_access.get();
        match family.as_str() {
            "Records" => {
                let declared_names = names_the_output_port_attribute_states(port)?;
                let record_payload =
                    records_message_payload_of(&port_name, payload, &declared_names)?;
                link_data_access.write_described_by(
                    python,
                    &port_name,
                    PyBytes::new(python, &record_payload).as_any(),
                    timestamp_ns,
                    starts_group,
                    |_| Ok(DataChannelPayloadDescriptor::records_message()),
                )
            }
            "Packets" => {
                let codec: Option<String> = port.getattr("codec")?.extract()?;
                let unit = PacketsUnitDescriptorBody {
                    codec: codec.unwrap_or_default(),
                };
                let descriptor = DataChannelPayloadDescriptor::packets_unit(&unit)
                    .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
                link_data_access.write_described_by(
                    python,
                    &port_name,
                    payload,
                    timestamp_ns,
                    starts_group,
                    |_| Ok(descriptor),
                )
            }
            "Channels" => {
                let start_sample = start_sample.ok_or_else(|| {
                    PyTypeError::new_err(format!(
                        "a write on Channels output port {port_name:?} names the block's first \
                         sample: write(self.{port_name}, samples, start_sample=…)"
                    ))
                })?;
                let sample_rate: u32 = port.getattr("rate")?.extract().map_err(|_| {
                    PyValueError::new_err(format!(
                        "Channels output port {port_name:?} states no rate on its class; write \
                         its blocks with write_channels_block(port, samples, start_sample=…, \
                         sample_rate=…)"
                    ))
                })?;
                let declared_shape = ChannelsOutputPortDeclaredShape {
                    sample_type: match port.getattr("dtype")?.extract::<Option<String>>()? {
                        None => None,
                        Some(dtype) if dtype == "f32" => Some(ChannelsBlockSampleType::F32),
                        Some(dtype) if dtype == "f64" => Some(ChannelsBlockSampleType::F64),
                        Some(dtype) => {
                            return Err(PyValueError::new_err(format!(
                                "Channels output port {port_name:?} declares dtype {dtype:?}, \
                                 which is neither \"f32\" nor \"f64\""
                            )));
                        }
                    },
                    channel_count: match names_the_output_port_attribute_states(port)?.len() {
                        0 => None,
                        stated => Some(u32::try_from(stated).map_err(|_| {
                            PyValueError::new_err(format!(
                                "Channels output port {port_name:?} states {stated} names"
                            ))
                        })?),
                    },
                };
                link_data_access.write_channels_block_to_output_port(
                    python,
                    &port_name,
                    payload,
                    start_sample,
                    sample_rate,
                    declared_shape,
                    timestamp_ns,
                    starts_group,
                )
            }
            "Image" => link_data_access.write_image_frame_to_output_port(
                python,
                &port_name,
                payload,
                &port.getattr("formats")?.extract::<Vec<String>>()?,
                self.gpu_limited_access_context.get(),
                timestamp_ns,
                starts_group,
            ),
            "Array" => {
                let declared_names = names_the_output_port_attribute_states(port)?;
                let declared_residency = match port
                    .getattr("residency")?
                    .extract::<Option<String>>()?
                {
                    None => None,
                    Some(residency) if residency == "gpu" => {
                        Some(ArrayOutputPortDeclaredResidency::Gpu)
                    }
                    Some(residency) if residency == "cpu" => {
                        Some(ArrayOutputPortDeclaredResidency::Cpu)
                    }
                    Some(other) => {
                        return Err(PyValueError::new_err(format!(
                            "Array output port {port_name:?} declares residency {other:?}, which \
                             is neither \"gpu\" nor \"cpu\""
                        )));
                    }
                };
                link_data_access.write_array_frame_to_output_port(
                    python,
                    &port_name,
                    payload,
                    &declared_names,
                    declared_residency,
                    self.gpu_limited_access_context.get(),
                    timestamp_ns,
                    starts_group,
                )
            }
            other => Err(PyTypeError::new_err(format!(
                "output port {port_name:?} declares family {other:?}, which is none of Image, \
                 Array, Channels, Packets, Records"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_attribute_of_an_input_subclass_names_its_port() {
        Python::initialize();
        Python::attach(|python| {
            let stream_distribution_source_root =
                concat!(env!("CARGO_MANIFEST_DIR"), "/../tatolab-stream");
            python
                .import("sys")
                .unwrap()
                .getattr("path")
                .unwrap()
                .call_method1("insert", (0, stream_distribution_source_root))
                .unwrap();
            let globals = pyo3::types::PyDict::new(python);
            python
                .run(
                    c"from tatolab.stream import Input, Records\n\
                      class EventsInput(Input):\n    pass\n\
                      class Holder:\n    events = EventsInput(Records, delivery='ordered')\n\
                      port = Holder.events\n",
                    Some(&globals),
                    None,
                )
                .unwrap();
            let port = globals.get_item("port").unwrap().unwrap();
            assert_eq!(port_attribute_name(&port, "Input").unwrap(), "events");
            let refusal = port_attribute_name(&port, "Output")
                .expect_err("an Input attribute names no output port");
            assert!(
                refusal.to_string().contains("got a EventsInput"),
                "{refusal}"
            );
        });
    }

    #[test]
    fn the_python_surface_spells_image_formats_as_the_engine_does() {
        Python::initialize();
        Python::attach(|python| {
            python
                .import("sys")
                .unwrap()
                .getattr("path")
                .unwrap()
                .call_method1(
                    "insert",
                    (0, concat!(env!("CARGO_MANIFEST_DIR"), "/../tatolab-stream")),
                )
                .unwrap();
            let python_image_formats: Vec<String> = python
                .import("tatolab.stream._port_family_declaration")
                .unwrap()
                .getattr("_IMAGE_TEXTURE_FORMATS")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(
                python_image_formats,
                streamlib::sdk::descriptors::IMAGE_TEXTURE_FORMAT_WIRE_NAMES
            );
        });
    }
}
