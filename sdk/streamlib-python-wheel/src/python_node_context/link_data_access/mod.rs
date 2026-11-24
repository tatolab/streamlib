// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use streamlib::sdk::iceoryx2::{
    ChannelsBlockSampleType, DataChannelPayloadDescriptor, PacketsUnitDescriptorBody,
};

use crate::python_bag_conversion::records_message_payload_of;
use crate::python_node_link_data_access::{
    ArrayOutputPortDeclaredResidency, ChannelsOutputPortDeclaredShape, PortFamiliesOfTheRead,
    PortFamilyOfTheRead, PortPayloadReadShape, PythonNodeLinkDataAccess,
};
use crate::python_port_payload::PythonPortPayloadLoan;

use super::gpu_context::PythonGpuContextLimitedAccess;

mod bundle_frame_reads_and_writes;

pub(crate) use bundle_frame_reads_and_writes::{
    read_the_bundle_frame_at_an_input_port, write_the_bundle_frame_on_an_output_port,
};

#[pyclass(name = "LinkInputDataReader", module = "tatolab.stream", frozen)]
pub(crate) struct PythonLinkInputDataReader {
    pub(super) link_data_access: Py<PythonNodeLinkDataAccess>,
    pub(super) gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    /// The bridge's blocking round trip to the parent, which holds the
    /// stream facts a read asks after.
    pub(super) escalate_request_to_parent: Option<Py<PyAny>>,
}

#[pymethods]
impl PythonLinkInputDataReader {
    /// The next payload on `port_name`, or `None`: the type of the family
    /// it arrived as among `families`, or the raw borrow when `families` is
    /// `None`.
    #[pyo3(signature = (port_name, *, families = None))]
    fn read<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        families: Option<Vec<String>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let read_shape = read_shape_of_the_families(port_name, families)?;
        self.link_data_access
            .get()
            .read_from_input_port_offering_gpu_access(
                python,
                port_name,
                read_shape,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message on `port_name` as a `Message` — payload, stamp, group
    /// flag, the messages its link lost since the previous read from it, and
    /// the link — or `None`. Its payload is read as `read` reads it.
    #[pyo3(signature = (port_name, *, families = None))]
    fn read_message<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        families: Option<Vec<String>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let read_shape = read_shape_of_the_families(port_name, families)?;
        self.link_data_access.get().read_message_from_input_port(
            python,
            port_name,
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

    /// The stream facts the output port feeding `port_name` carries now, as
    /// `tatolab.stream.stream_facts`'s types, or `None`.
    ///
    /// A port fed by more than one link names the one to read with
    /// `inbound_link`.
    #[pyo3(signature = (port_name, *, inbound_link = None))]
    fn upstream_stream_facts<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        inbound_link: Option<&str>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let op = PyDict::new(python);
        op.set_item("op", "read_inbound_link_stream_facts")?;
        op.set_item("port", port_name)?;
        if let Some(inbound_link) = inbound_link {
            op.set_item("inbound_link", inbound_link)?;
        }
        let response = stream_facts_round_trip_to_parent(
            python,
            self.escalate_request_to_parent.as_ref(),
            &op,
        )?;
        let stream_facts_wire = response.call_method1("get", ("stream_facts",))?;
        if stream_facts_wire.is_none() {
            return Ok(None);
        }
        python
            .import("tatolab.stream.stream_facts")?
            .call_method1("stream_facts_from_wire", (stream_facts_wire,))
            .map(Some)
    }
}

/// What a read of `port_name` hands back: the type of the family a message
/// arrived as, the first of `families` the one the port declares, or the raw
/// borrow when `families` is `None`.
fn read_shape_of_the_families(
    port_name: &str,
    families: Option<Vec<String>>,
) -> PyResult<PortPayloadReadShape> {
    let Some(families) = families else {
        return Ok(PortPayloadReadShape::RawBorrow);
    };
    let mut families = families
        .iter()
        .map(|family| port_family_named(port_name, family))
        .collect::<PyResult<Vec<_>>>()?
        .into_iter();
    let declared_family = families.next().ok_or_else(|| {
        PyValueError::new_err(format!(
            "a read of input port {port_name:?} names no family to read it as"
        ))
    })?;
    Ok(PortPayloadReadShape::FamilyType(
        PortFamiliesOfTheRead::of_the_declared_family_and_also(declared_family, families),
    ))
}

/// The family `family` names on the wire, such as `image`.
fn port_family_named(port_name: &str, family: &str) -> PyResult<PortFamilyOfTheRead> {
    PortFamilyOfTheRead::of_wire_name(family).ok_or_else(|| {
        PyValueError::new_err(format!(
            "port {port_name:?} is read or written as family {family:?}, which is none of \
             image, array, channels, packets, records"
        ))
    })
}

#[pyclass(name = "LinkOutputDataWriter", module = "tatolab.stream", frozen)]
pub(crate) struct PythonLinkOutputDataWriter {
    pub(super) link_data_access: Py<PythonNodeLinkDataAccess>,
    /// What names the descriptor of a pooled GPU frame a write carries.
    pub(super) gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    pub(super) escalate_request_to_parent: Option<Py<PyAny>>,
}

#[pymethods]
impl PythonLinkOutputDataWriter {
    /// Publish one message to every downstream link on `port_name`: a copy of
    /// `payload`, any contiguous buffer, when `family` is `None`, else the
    /// family's type, written as the port's `codec`, `formats`, `names`,
    /// `sample_type` and `residency` state it.
    #[pyo3(signature = (
        port_name,
        payload,
        timestamp_ns = None,
        starts_group = true,
        *,
        family = None,
        codec = None,
        formats = None,
        names = None,
        sample_type = None,
        residency = None,
        start_sample = None,
        sample_rate = None,
    ))]
    #[allow(
        clippy::too_many_arguments,
        reason = "each keyword is one fact of the port's family a write states"
    )]
    fn write(
        &self,
        python: Python<'_>,
        port_name: &str,
        payload: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
        starts_group: bool,
        family: Option<&str>,
        codec: Option<String>,
        formats: Option<Vec<String>>,
        names: Option<Vec<String>>,
        sample_type: Option<&str>,
        residency: Option<&str>,
        start_sample: Option<u64>,
        sample_rate: Option<u32>,
    ) -> PyResult<()> {
        let link_data_access = self.link_data_access.get();
        let Some(family) = family else {
            if start_sample.is_some() || sample_rate.is_some() {
                return Err(PyTypeError::new_err(format!(
                    "start_sample= and sample_rate= belong to a write of output port \
                     {port_name:?} as family \"channels\""
                )));
            }
            return link_data_access.write_described_by(
                python,
                port_name,
                payload,
                timestamp_ns,
                starts_group,
                |_payload_bytes| Ok(DataChannelPayloadDescriptor::bytes()),
            );
        };
        let names = names.unwrap_or_default();
        match port_family_named(port_name, family)? {
            PortFamilyOfTheRead::Records => {
                let record_payload = records_message_payload_of(port_name, payload, &names)?;
                link_data_access.write_described_by(
                    python,
                    port_name,
                    PyBytes::new(python, &record_payload).as_any(),
                    timestamp_ns,
                    starts_group,
                    |_| Ok(DataChannelPayloadDescriptor::records_message()),
                )
            }
            PortFamilyOfTheRead::Packets => {
                let unit = PacketsUnitDescriptorBody {
                    codec: codec.unwrap_or_default(),
                };
                let descriptor = DataChannelPayloadDescriptor::packets_unit(&unit)
                    .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
                link_data_access.write_described_by(
                    python,
                    port_name,
                    payload,
                    timestamp_ns,
                    starts_group,
                    |_| Ok(descriptor),
                )
            }
            PortFamilyOfTheRead::Channels => {
                let (Some(start_sample), Some(sample_rate)) = (start_sample, sample_rate) else {
                    return Err(PyTypeError::new_err(format!(
                        "a write on Channels output port {port_name:?} names the block's first \
                         sample and its rate: start_sample=…, sample_rate=…"
                    )));
                };
                let declared_shape = ChannelsOutputPortDeclaredShape {
                    sample_type: match sample_type {
                        None => None,
                        Some("f32") => Some(ChannelsBlockSampleType::F32),
                        Some("f64") => Some(ChannelsBlockSampleType::F64),
                        Some(other) => {
                            return Err(PyValueError::new_err(format!(
                                "Channels output port {port_name:?} is written as {other:?}, \
                                 which is neither \"f32\" nor \"f64\""
                            )));
                        }
                    },
                    channel_count: match names.len() {
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
                    port_name,
                    payload,
                    start_sample,
                    sample_rate,
                    declared_shape,
                    timestamp_ns,
                    starts_group,
                )
            }
            PortFamilyOfTheRead::Image => link_data_access.write_image_frame_to_output_port(
                python,
                port_name,
                payload,
                &formats.unwrap_or_default(),
                self.gpu_limited_access_context.get(),
                timestamp_ns,
                starts_group,
            ),
            PortFamilyOfTheRead::Array => {
                let declared_residency = match residency {
                    None => None,
                    Some("gpu") => Some(ArrayOutputPortDeclaredResidency::Gpu),
                    Some("cpu") => Some(ArrayOutputPortDeclaredResidency::Cpu),
                    Some(other) => {
                        return Err(PyValueError::new_err(format!(
                            "Array output port {port_name:?} is written as residency {other:?}, \
                             which is neither \"gpu\" nor \"cpu\""
                        )));
                    }
                };
                link_data_access.write_array_frame_to_output_port(
                    python,
                    port_name,
                    payload,
                    &names,
                    declared_residency,
                    self.gpu_limited_access_context.get(),
                    timestamp_ns,
                    starts_group,
                )
            }
        }
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
    /// `with pin.loan(n) as out:`; the sample is published when
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

    /// Replace the stream facts `port_name` carries with `stream_facts`, one of
    /// `tatolab.stream.stream_facts`'s types. The parent refuses facts of
    /// another family than the port's.
    fn set_stream_facts(
        &self,
        python: Python<'_>,
        port_name: &str,
        stream_facts: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let stream_facts_wire = stream_facts.call_method0("to_wire").map_err(|_| {
            PyTypeError::new_err(format!(
                "output port {port_name:?}: set_stream_facts takes ImageStreamFacts or \
                 PacketsStreamFacts from tatolab.stream.stream_facts, got {}",
                stream_facts
                    .get_type()
                    .name()
                    .map(|type_name| type_name.to_string())
                    .unwrap_or_default()
            ))
        })?;
        let op = PyDict::new(python);
        op.set_item("op", "set_output_port_stream_facts")?;
        op.set_item("port", port_name)?;
        op.set_item("stream_facts", stream_facts_wire)?;
        stream_facts_round_trip_to_parent(python, self.escalate_request_to_parent.as_ref(), &op)?;
        Ok(())
    }
}

/// One stream-facts escalate round trip to the parent, whose refusal raises
/// as the bridge raises it.
fn stream_facts_round_trip_to_parent<'py>(
    python: Python<'py>,
    escalate_request_to_parent: Option<&Py<PyAny>>,
    op: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    escalate_request_to_parent
        .ok_or_else(|| {
            PyRuntimeError::new_err(
                "stream facts live on the parent's graph, and this context has no route to the \
                 parent",
            )
        })?
        .bind(python)
        .call1((op,))
}

#[cfg(test)]
mod tests {
    use super::*;

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
