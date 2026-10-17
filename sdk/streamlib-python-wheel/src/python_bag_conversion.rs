// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python data across the boundary: a tapped channel's msgpack message decoded
//! for inspection, config and graph JSON both ways, and the cast a typed read
//! hands a message's payload bytes to. A message's payload otherwise crosses
//! as the bytes it is; nothing here encodes one.

use std::cell::RefCell;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};
use rmpv::Value;

use streamlib::sdk::iceoryx2::{FrameHeader, TappedFramePayloadRefusal};

use crate::python_processor_context::PythonGpuContextLimitedAccess;

/// Decode msgpack bytes into ordinary Python data.
pub(crate) fn decode_msgpack_to_python_object<'py>(
    python: Python<'py>,
    encoded: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let value = rmpv::decode::read_value_with_max_depth(
        &mut &encoded[..],
        RMPV_DEPTH_ADMITTING_THE_MAXIMUM_NESTED_CONTAINERS,
    )
    .map_err(|decode_failure| match decode_failure {
        rmpv::decode::Error::DepthLimitExceeded => {
            refusal_of_decoded_data_nested_past_the_maximum()
        }
        decode_failure => PyValueError::new_err(decode_failure.to_string()),
    })?;
    msgpack_value_to_python_object(python, &value, ContainersEnclosingAValue::OUTERMOST)
}

/// Decode one raw tapped-channel message, msgpack-encoded, into ordinary Python
/// data.
///
/// A tap forwards the channel's wire bytes verbatim, header included, so the
/// transport's own accessor is what bounds the msgpack value inside them.
#[pyfunction]
pub(crate) fn decode_tapped_channel_bag_frame_to_python_object<'py>(
    python: Python<'py>,
    framed_bag_bytes: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let payload =
        FrameHeader::payload_bounded_by_its_header(framed_bag_bytes).map_err(|refusal| {
            PyValueError::new_err(TappedFramePayloadRefusal::from(refusal).to_string())
        })?;

    decode_msgpack_to_python_object(python, payload)
}

/// Hand one message's payload bytes to the `from_bytes` of the target an author
/// named with `read(port, into=T)`, and return what it built.
///
/// Whatever `from_bytes` raises is what the author sees: a pydantic
/// `ValidationError` or a dataclass's missing-argument `TypeError` says more
/// about the mismatch than any wrapper here could.
///
/// `offered_gpu_limited_access` is what the cast may reach for through
/// [`gpu_limited_access_of_the_typed_read_in_progress`] — offered for the
/// `from_bytes` call and withdrawn the moment it returns. Nothing here knows
/// which targets want it or what they do with it.
pub(crate) fn cast_payload_bytes_into_read_target<'py>(
    port_name: &str,
    payload_bytes: &Bound<'py, PyBytes>,
    read_target: &Bound<'py, PyAny>,
    offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
) -> PyResult<Bound<'py, PyAny>> {
    let python = read_target.py();
    let Some(from_bytes) = read_target
        .getattr_opt(intern!(python, "from_bytes"))?
        .filter(|from_bytes| from_bytes.is_callable())
    else {
        return Err(PyTypeError::new_err(format!(
            "`into=` on input port {port_name:?} was handed a {}, which has no `from_bytes` to \
             build the read from the message's payload bytes — name a class with a `from_bytes` \
             classmethod (`tatolab.stream.MsgpackCast` gives one) or \
             `tatolab.stream.msgpack_typed_dict_cast(SomeTypedDict)`.",
            name_of_the_read_target_for_error_message(read_target)
        )));
    };
    let _offer = TypedReadGpuAccessOffer::open(offered_gpu_limited_access);
    from_bytes.call1((payload_bytes,))
}

/// The name a refusal gives an `into=` target: a class by its own name, any
/// other object by its type's.
fn name_of_the_read_target_for_error_message(read_target: &Bound<'_, PyAny>) -> String {
    match read_target.cast::<PyType>() {
        Ok(read_target_class) => read_target_class.name().map_or_else(
            |_| "class".to_string(),
            |class_name| format!("class {class_name}"),
        ),
        Err(_) => python_type_name_for_error_message(read_target, "value"),
    }
}

// =============================================================================
// What a typed read offers the type it is constructing
// =============================================================================

thread_local! {
    /// The GPU capability the typed read running on this thread is offering,
    /// and only while it is constructing.
    ///
    /// Thread-local rather than passed: `read(port, into=T)` builds its result
    /// with `T.from_bytes(payload)`, which is the whole contract — an argument
    /// the engine added would be one every target had to accept, and most
    /// targets are ordinary records that want nothing to do with a GPU.
    ///
    /// What this is keyed on is what it guarantees: one OS thread, one read at
    /// a time. A processor's callbacks run on its own dedicated thread, so the
    /// window is exactly one `from_bytes` call deep. The green-thread execution
    /// flavor the plan keeps OPEN (ARCHITECTURE.md §Processor model) would
    /// break that — a switch inside `from_bytes` is not nested reentrancy, and a
    /// resumed read would see the wrong offer — so that flavor owes this a
    /// rework onto `contextvars`, which follows the switch.
    static GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ:
        RefCell<Option<Py<PythonGpuContextLimitedAccess>>> = const { RefCell::new(None) };
}

/// Opens the offer for one `from_bytes` call and withdraws it on drop, whether
/// it returned or raised.
struct TypedReadGpuAccessOffer {
    /// Restored rather than cleared: a target whose `from_bytes` reads its own
    /// input ports would otherwise leave the outer read's offer withdrawn.
    offer_this_read_displaced: Option<Py<PythonGpuContextLimitedAccess>>,
}

impl TypedReadGpuAccessOffer {
    fn open(offered_gpu_limited_access: Option<&Bound<'_, PythonGpuContextLimitedAccess>>) -> Self {
        let offered = offered_gpu_limited_access.map(|capability| capability.clone().unbind());
        Self {
            offer_this_read_displaced: GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ
                .with(|offer| offer.replace(offered)),
        }
    }
}

impl Drop for TypedReadGpuAccessOffer {
    /// `replace` rather than an assignment through `borrow_mut`, so the
    /// displaced capability drops after the borrow ends: a decref that ran
    /// Python and re-entered the reader below would otherwise panic on an
    /// already-mutable borrow.
    fn drop(&mut self) {
        let restored = self.offer_this_read_displaced.take();
        GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ.with(|offer| offer.replace(restored));
    }
}

/// The GPU capability of the `read(port, into=T)` currently running
/// `T.from_bytes`, or `None` when nothing is being read into a type.
///
/// The same capability as `ctx.gpu_limited_access`, offered so a type can do
/// per-frame work as it is built that needs the engine — claiming the frame's
/// surface against producer reuse is what the shipped `VideoFrame` does with
/// it. Any class reachable through `into=` may call this; there is no
/// registration, no marker and no privileged type.
#[pyfunction]
pub(crate) fn gpu_limited_access_of_the_typed_read_in_progress(
    python: Python<'_>,
) -> Option<Py<PythonGpuContextLimitedAccess>> {
    GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ.with(|offer| {
        offer
            .borrow()
            .as_ref()
            .map(|capability| capability.clone_ref(python))
    })
}

/// The type name to name in a refusal, or `unknown_type_placeholder` when even
/// that cannot be read.
pub(crate) fn python_type_name_for_error_message(
    value: &Bound<'_, PyAny>,
    unknown_type_placeholder: &str,
) -> String {
    value.get_type().name().map_or_else(
        |_| unknown_type_placeholder.to_string(),
        |type_name| type_name.to_string(),
    )
}

/// Convert JSON into ordinary Python data.
///
/// Routed through the msgpack value tree the tap decode uses rather than
/// growing a second converter: one conversion is one set of edge cases.
pub(crate) fn json_value_to_python_object<'py>(
    python: Python<'py>,
    json_value: &serde_json::Value,
) -> PyResult<Bound<'py, PyAny>> {
    let value = rmpv::ext::to_value(json_value)
        .map_err(|convert_failure| PyValueError::new_err(convert_failure.to_string()))?;
    msgpack_value_to_python_object(python, &value, ContainersEnclosingAValue::OUTERMOST)
}

/// Convert ordinary Python data into JSON, refusing what JSON cannot carry —
/// `bytes`, or a msgpack extension value.
pub(crate) fn python_object_to_json_value(
    python_data: &Bound<'_, PyAny>,
) -> PyResult<serde_json::Value> {
    let value = python_object_to_msgpack_value(python_data, ContainersEnclosingAValue::OUTERMOST)?;
    rmpv::ext::from_value(value).map_err(|convert_failure| {
        PyTypeError::new_err(format!(
            "the value must survive a JSON round trip: {convert_failure} — carry `bytes` as a \
             `str` or a list of ints"
        ))
    })
}

/// The most containers one value may nest, the outermost counted, as it
/// crosses the Python boundary in either direction. The walk recurses once per
/// container, so the bound keeps it inside the thread's stack and turns a
/// container that holds itself into a refusal.
const MAXIMUM_NESTED_CONTAINER_DEPTH: usize = 128;

/// rmpv's depth budget that admits every value nesting at most
/// [`MAXIMUM_NESTED_CONTAINER_DEPTH`] containers: rmpv 1.3 spends two levels on
/// each container and at most three on the leaf inside the innermost, so a
/// value that exhausts it nests past the maximum, and rmpv's recursion stays
/// as shallow as the bound.
const RMPV_DEPTH_ADMITTING_THE_MAXIMUM_NESTED_CONTAINERS: usize =
    2 * MAXIMUM_NESTED_CONTAINER_DEPTH + 3;

/// How many containers enclose a value being converted across the Python
/// boundary.
#[derive(Clone, Copy)]
struct ContainersEnclosingAValue(usize);

impl ContainersEnclosingAValue {
    /// The value a conversion starts at, which no container encloses.
    const OUTERMOST: Self = Self(0);

    /// The count for the entries of a container this many containers enclose,
    /// or `refusal_past_the_maximum` once the container itself sits
    /// [`MAXIMUM_NESTED_CONTAINER_DEPTH`] containers deep.
    fn entered_one_more_container(self, refusal_past_the_maximum: fn() -> PyErr) -> PyResult<Self> {
        if self.0 < MAXIMUM_NESTED_CONTAINER_DEPTH {
            Ok(Self(self.0 + 1))
        } else {
            Err(refusal_past_the_maximum())
        }
    }
}

/// The refusal of Python data whose containers nest past
/// [`MAXIMUM_NESTED_CONTAINER_DEPTH`].
fn refusal_of_python_data_nested_past_the_maximum() -> PyErr {
    PyValueError::new_err(format!(
        "containers nest more than {MAXIMUM_NESTED_CONTAINER_DEPTH} deep, the most one value may \
         — a dict, list or tuple that holds itself nests without end. Break the cycle, or nest \
         the data at most {MAXIMUM_NESTED_CONTAINER_DEPTH} containers deep"
    ))
}

/// The refusal of a value bound for Python whose containers nest past
/// [`MAXIMUM_NESTED_CONTAINER_DEPTH`].
fn refusal_of_decoded_data_nested_past_the_maximum() -> PyErr {
    PyValueError::new_err(format!(
        "containers nest more than {MAXIMUM_NESTED_CONTAINER_DEPTH} deep in this value, the most \
         one value may cross into Python. Have its producer nest the data at most \
         {MAXIMUM_NESTED_CONTAINER_DEPTH} containers deep"
    ))
}

fn python_object_to_msgpack_value(
    value: &Bound<'_, PyAny>,
    containers_enclosing_it: ContainersEnclosingAValue,
) -> PyResult<Value> {
    if value.is_none() {
        return Ok(Value::Nil);
    }
    // Before the integer check: `bool` is a subclass of `int` in Python, so the
    // order here is what keeps `True` from encoding as `1`.
    if let Ok(boolean) = value.cast::<PyBool>() {
        return Ok(Value::Boolean(boolean.is_true()));
    }
    if let Ok(integer) = value.cast::<PyInt>() {
        return integer
            .extract::<i64>()
            .map(Value::from)
            .or_else(|_| integer.extract::<u64>().map(Value::from))
            .map_err(|_| {
                PyValueError::new_err(
                    "integer does not fit in 64 bits — msgpack carries no wider integer",
                )
            });
    }
    if let Ok(float) = value.cast::<PyFloat>() {
        return Ok(Value::from(float.value()));
    }
    if let Ok(text) = value.cast::<PyString>() {
        return Ok(Value::from(text.to_cow()?.into_owned()));
    }
    if let Ok(bytes) = value.cast::<PyBytes>() {
        return Ok(Value::Binary(bytes.as_bytes().to_vec()));
    }
    if let Ok(mapping) = value.cast::<PyDict>() {
        return named_map_to_msgpack_value(mapping, containers_enclosing_it);
    }
    if let Ok(sequence) = value.cast::<PyList>() {
        return sequence_to_msgpack_array(sequence.iter(), containers_enclosing_it);
    }
    if let Ok(sequence) = value.cast::<PyTuple>() {
        return sequence_to_msgpack_array(sequence.iter(), containers_enclosing_it);
    }
    Err(PyTypeError::new_err(format!(
        "cannot convert a {}: a value is built from dict, list, tuple, str, bytes, int, float, \
         bool and None",
        value.get_type().name()?
    )))
}

/// The keys a decoded msgpack extension value is carried under, so a tapped
/// message holding somebody else's extension type reads as data a reader can
/// look at.
const EXTENSION_TYPE_KEY: &str = "__msgpack_ext_type__";
const EXTENSION_DATA_KEY: &str = "__msgpack_ext_data__";

/// Convert a dict, requiring string keys at every level.
fn named_map_to_msgpack_value(
    mapping: &Bound<'_, PyDict>,
    containers_enclosing_the_mapping: ContainersEnclosingAValue,
) -> PyResult<Value> {
    let containers_enclosing_the_entries = containers_enclosing_the_mapping
        .entered_one_more_container(refusal_of_python_data_nested_past_the_maximum)?;
    let mut entries = Vec::with_capacity(mapping.len());
    for (key, entry) in mapping.iter() {
        let key = key.cast::<PyString>().map_err(|_| {
            PyTypeError::new_err(format!(
                "keys must be strings; got a {} key",
                python_type_name_for_error_message(&key, "non-string")
            ))
        })?;
        entries.push((
            Value::from(key.to_cow()?.into_owned()),
            python_object_to_msgpack_value(&entry, containers_enclosing_the_entries)?,
        ));
    }
    Ok(Value::Map(entries))
}

fn sequence_to_msgpack_array<'py>(
    items: impl Iterator<Item = Bound<'py, PyAny>>,
    containers_enclosing_the_sequence: ContainersEnclosingAValue,
) -> PyResult<Value> {
    let containers_enclosing_the_items = containers_enclosing_the_sequence
        .entered_one_more_container(refusal_of_python_data_nested_past_the_maximum)?;
    items
        .map(|item| python_object_to_msgpack_value(&item, containers_enclosing_the_items))
        .collect::<PyResult<Vec<Value>>>()
        .map(Value::Array)
}

fn msgpack_value_to_python_object<'py>(
    python: Python<'py>,
    value: &Value,
    containers_enclosing_it: ContainersEnclosingAValue,
) -> PyResult<Bound<'py, PyAny>> {
    let object = match value {
        Value::Nil => python.None().into_bound(python),
        Value::Boolean(boolean) => boolean.into_pyobject(python)?.to_owned().into_any(),
        Value::Integer(integer) => match (integer.as_i64(), integer.as_u64()) {
            (Some(signed), _) => signed.into_pyobject(python)?.into_any(),
            (None, Some(unsigned)) => unsigned.into_pyobject(python)?.into_any(),
            (None, None) => {
                return Err(PyValueError::new_err(
                    "integer on the wire fits neither i64 nor u64",
                ));
            }
        },
        Value::F32(float) => float.into_pyobject(python)?.into_any(),
        Value::F64(float) => float.into_pyobject(python)?.into_any(),
        Value::String(text) => match text.as_str() {
            Some(text) => text.into_pyobject(python)?.into_any(),
            // msgpack strings are not required to be UTF-8; the raw bytes are
            // the honest reading, and losing them to a lossy decode would be
            // worse than handing back `bytes`.
            None => PyBytes::new(python, text.as_bytes()).into_any(),
        },
        Value::Binary(bytes) => PyBytes::new(python, bytes).into_any(),
        Value::Array(items) => {
            let containers_enclosing_the_items = containers_enclosing_it
                .entered_one_more_container(refusal_of_decoded_data_nested_past_the_maximum)?;
            let converted = items
                .iter()
                .map(|item| {
                    msgpack_value_to_python_object(python, item, containers_enclosing_the_items)
                })
                .collect::<PyResult<Vec<_>>>()?;
            PyList::new(python, converted)?.into_any()
        }
        Value::Map(entries) => {
            let containers_enclosing_the_entries = containers_enclosing_it
                .entered_one_more_container(refusal_of_decoded_data_nested_past_the_maximum)?;
            let mapping = PyDict::new(python);
            for (key, entry) in entries {
                mapping.set_item(
                    msgpack_value_to_python_object(python, key, containers_enclosing_the_entries)?,
                    msgpack_value_to_python_object(
                        python,
                        entry,
                        containers_enclosing_the_entries,
                    )?,
                )?;
            }
            mapping.into_any()
        }
        Value::Ext(type_tag, bytes) => {
            let extension = PyDict::new(python);
            extension.set_item(EXTENSION_TYPE_KEY, type_tag)?;
            extension.set_item(EXTENSION_DATA_KEY, PyBytes::new(python, bytes))?;
            extension.into_any()
        }
    };
    Ok(object)
}

#[cfg(test)]
mod tests {
    use streamlib::sdk::iceoryx2::{FRAME_HEADER_SIZE, MessageGroupBoundary};

    use super::*;

    fn msgpack_bytes_of(value: &Value) -> Vec<u8> {
        let mut encoded = Vec::new();
        rmpv::encode::write_value(&mut encoded, value).unwrap();
        encoded
    }

    fn msgpack_map_of(entries: Vec<(&str, Value)>) -> Value {
        Value::Map(
            entries
                .into_iter()
                .map(|(key, entry)| (Value::from(key), entry))
                .collect(),
        )
    }

    /// No slack behind the payload — the frame ends where the message does.
    const SLICE_HOLDS_ONLY_THE_MESSAGE: usize = 0;

    /// How many bytes the truncation fixture cuts off the end of a whole frame.
    const BYTES_THE_PREVIEW_CUT: usize = 8;

    /// Frame a msgpack payload the way the transport does, in a buffer of at
    /// least `slice_capacity_at_least` — a caller may hold the frame in
    /// something larger than the frame.
    fn frame_like_the_transport(payload: &[u8], slice_capacity_at_least: usize) -> Vec<u8> {
        let mut framed = vec![0u8; slice_capacity_at_least.max(FRAME_HEADER_SIZE + payload.len())];
        FrameHeader::new(
            "processor/port",
            1_234,
            payload.len() as u32,
            MessageGroupBoundary::StartsGroup,
        )
        .expect("port key fits")
        .write_to_slice(&mut framed);
        framed[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + payload.len()].copy_from_slice(payload);
        framed
    }

    /// The whole job end to end: strip the transport header and decode the
    /// declared payload. The bound itself is pinned in `streamlib-ipc-types`;
    /// what the oversized buffer proves here is that a frame held in one does
    /// not derail the decode.
    #[test]
    fn a_tapped_message_decodes_to_what_the_channel_published() {
        Python::initialize();
        Python::attach(|python| {
            let payload = msgpack_bytes_of(&msgpack_map_of(vec![
                ("surface_id", Value::from("camera/frame#7")),
                ("width", Value::from(1920)),
            ]));

            let framed = frame_like_the_transport(&payload, 4096);
            let decoded =
                decode_tapped_channel_bag_frame_to_python_object(python, &framed).unwrap();

            assert_eq!(
                FrameHeader::read_payload_from_slice(&framed).map(<[u8]>::len),
                Some(payload.len()),
                "the buffer's trailing bytes were handed to the decoder"
            );
            let published = PyDict::new(python);
            published.set_item("surface_id", "camera/frame#7").unwrap();
            published.set_item("width", 1920i64).unwrap();
            assert!(
                decoded.eq(&published).unwrap(),
                "the decode changed the message"
            );
        });
    }

    /// The tap tool hex-previews only the first bytes of a large message.
    /// Decoding that prefix must refuse by name: msgpack would otherwise happily
    /// read the leading map entries and hand back a map missing its later
    /// fields.
    #[test]
    fn a_truncated_sample_is_refused_rather_than_half_decoded() {
        Python::initialize();
        Python::attach(|python| {
            let payload = msgpack_bytes_of(&msgpack_map_of(vec![
                ("surface_id", Value::from("camera/frame#7")),
                ("filler", Value::from("x".repeat(512))),
            ]));

            let framed = frame_like_the_transport(&payload, SLICE_HOLDS_ONLY_THE_MESSAGE);
            let truncated = &framed[..framed.len() - BYTES_THE_PREVIEW_CUT];

            let refusal = decode_tapped_channel_bag_frame_to_python_object(python, truncated)
                .expect_err("a truncated sample must not decode");
            assert!(
                refusal.to_string().contains("truncated"),
                "the refusal must name the truncation; got {refusal}"
            );
        });
    }

    /// Fewer bytes than a header is not a zero-length message — it is a caller
    /// handing over something that was never a frame.
    #[test]
    fn bytes_too_short_to_hold_a_header_are_refused() {
        Python::initialize();
        Python::attach(|python| {
            let refusal = decode_tapped_channel_bag_frame_to_python_object(python, &[0u8; 8])
                .expect_err("8 bytes cannot be a framed message");
            assert!(refusal.to_string().contains("frame header"));
        });
    }

    /// Every msgpack value shape decodes into the Python type a reader of a
    /// tapped message expects: `bin` as `bytes`, a boolean as `bool` rather
    /// than an `int`, an array as a list, and an extension value as its type
    /// and data.
    #[test]
    fn every_msgpack_value_shape_decodes_into_ordinary_python_data() {
        Python::initialize();
        Python::attach(|python| {
            let encoded = msgpack_bytes_of(&msgpack_map_of(vec![
                ("nothing", Value::Nil),
                ("flag", Value::from(true)),
                ("count", Value::from(-42)),
                ("huge", Value::from(u64::MAX)),
                ("ratio", Value::from(1.5f64)),
                ("label", Value::from("カメラ 🎥")),
                ("payload", Value::Binary(vec![0, 255, 7])),
                (
                    "items",
                    Value::Array(vec![Value::from(1), Value::from(2), Value::from(3)]),
                ),
                (
                    "nested",
                    msgpack_map_of(vec![("inner", Value::from("value"))]),
                ),
                ("extension", Value::Ext(42, vec![1, 2, 3])),
            ]));

            let decoded = decode_msgpack_to_python_object(python, &encoded).unwrap();

            let expected = PyDict::new(python);
            expected.set_item("nothing", python.None()).unwrap();
            expected.set_item("flag", true).unwrap();
            expected.set_item("count", -42i64).unwrap();
            expected.set_item("huge", u64::MAX).unwrap();
            expected.set_item("ratio", 1.5f64).unwrap();
            expected.set_item("label", "カメラ 🎥").unwrap();
            expected
                .set_item("payload", PyBytes::new(python, &[0u8, 255, 7]))
                .unwrap();
            expected.set_item("items", vec![1i64, 2, 3]).unwrap();
            let nested = PyDict::new(python);
            nested.set_item("inner", "value").unwrap();
            expected.set_item("nested", &nested).unwrap();
            let extension = PyDict::new(python);
            extension.set_item(EXTENSION_TYPE_KEY, 42i64).unwrap();
            extension
                .set_item(EXTENSION_DATA_KEY, PyBytes::new(python, &[1u8, 2, 3]))
                .unwrap();
            expected.set_item("extension", &extension).unwrap();
            assert!(
                decoded.eq(&expected).unwrap(),
                "decoded {decoded}, expected {expected}"
            );
            assert!(decoded.get_item("flag").unwrap().is_instance_of::<PyBool>());
            assert!(
                decoded
                    .get_item("payload")
                    .unwrap()
                    .is_instance_of::<PyBytes>()
            );
        });
    }

    /// `bool` is a subclass of `int` in Python, so a naive integer-first check
    /// silently turns `True` into `1`.
    #[test]
    fn booleans_do_not_decay_into_integers_on_the_way_to_json() {
        Python::initialize();
        Python::attach(|python| {
            let source = PyDict::new(python);
            source.set_item("flag", true).unwrap();
            assert_eq!(
                python_object_to_json_value(source.as_any()).unwrap(),
                serde_json::json!({"flag": true})
            );
        });
    }

    /// A tuple is a reasonable thing for an author to write, and JSON has no
    /// tuple — it converts as an array rather than being refused.
    #[test]
    fn a_tuple_is_accepted_and_converts_as_an_array() {
        Python::initialize();
        Python::attach(|python| {
            let source = PyDict::new(python);
            source
                .set_item("items", PyTuple::new(python, [1i64, 2, 3]).unwrap())
                .unwrap();
            assert_eq!(
                python_object_to_json_value(source.as_any()).unwrap(),
                serde_json::json!({"items": [1, 2, 3]})
            );
        });
    }

    /// The refusal names what a value may hold — an author who reaches for a
    /// numpy array or a socket should learn the rule from the error.
    #[test]
    fn an_unconvertible_object_is_refused_with_the_rule() {
        Python::initialize();
        Python::attach(|python| {
            let unconvertible = python
                .import("builtins")
                .unwrap()
                .getattr("object")
                .unwrap()
                .call0()
                .unwrap();
            let source = PyDict::new(python);
            source.set_item("payload", unconvertible).unwrap();
            let failure = python_object_to_json_value(source.as_any()).unwrap_err();
            assert!(
                failure.to_string().contains("a value is built from"),
                "the refusal must state what is allowed, got: {failure}"
            );
        });
    }

    /// A key JSON cannot spell is refused at every level, not just the top.
    #[test]
    fn a_key_that_is_not_a_string_is_refused_at_any_depth() {
        Python::initialize();
        Python::attach(|python| {
            let int_keyed = PyDict::new(python);
            int_keyed.set_item(1i64, "value").unwrap();
            let failure = python_object_to_json_value(int_keyed.as_any()).unwrap_err();
            assert!(
                failure.to_string().contains("keys must be strings"),
                "got: {failure}"
            );

            let nested_int_keyed = PyDict::new(python);
            nested_int_keyed.set_item("inner", &int_keyed).unwrap();
            assert!(python_object_to_json_value(nested_int_keyed.as_any()).is_err());
        });
    }

    /// Build a target the way an author's module would, so the runtime carries
    /// whatever CPython actually assigns rather than attributes a test set by
    /// hand.
    fn read_target_from_source<'py>(
        python: Python<'py>,
        source: &str,
        target_name: &str,
    ) -> Bound<'py, PyAny> {
        let namespace = PyDict::new(python);
        python
            .run(
                &std::ffi::CString::new(source).unwrap(),
                Some(&namespace),
                None,
            )
            .unwrap();
        namespace.get_item(target_name).unwrap().unwrap()
    }

    const DETECTION_CLASS_READ_FROM_ITS_LABEL_BYTES: &str = "\
class Detection:
    def __init__(self, label):
        self.label = label

    @classmethod
    def from_bytes(cls, payload):
        return cls(payload.decode('utf-8'))
";

    /// A class with a `from_bytes` classmethod is handed the payload exactly as
    /// it crossed the wire, and what it returns is the read.
    #[test]
    fn a_class_with_from_bytes_is_handed_the_payload_bytes() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                DETECTION_CLASS_READ_FROM_ITS_LABEL_BYTES,
                "Detection",
            );
            let read = cast_payload_bytes_into_read_target(
                "detections",
                &PyBytes::new(python, b"cat"),
                &target,
                None,
            )
            .unwrap();
            assert!(read.is_instance(&target).unwrap());
            assert_eq!(
                read.getattr("label").unwrap().extract::<String>().unwrap(),
                "cat"
            );
        });
    }

    /// `into=` takes any object with a `from_bytes`, not only a class — the
    /// shape `msgpack_typed_dict_cast` returns.
    #[test]
    fn an_object_with_from_bytes_casts_like_a_class() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                "class LengthOfThePayload:\n    def from_bytes(self, payload):\n        return \
                 len(payload)\nlength_of_the_payload = LengthOfThePayload()\n",
                "length_of_the_payload",
            );
            let read = cast_payload_bytes_into_read_target(
                "detections",
                &PyBytes::new(python, b"four"),
                &target,
                None,
            )
            .unwrap();
            assert_eq!(read.extract::<usize>().unwrap(), 4);
        });
    }

    /// The dial's whole point: a payload the target cannot be built from raises
    /// at the consuming read, and what `from_bytes` raised is what the author
    /// sees.
    #[test]
    fn what_from_bytes_raises_reaches_the_author_unchanged() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                "class Detection:\n    @classmethod\n    def from_bytes(cls, payload):\n        \
                 raise ValueError(f'no detection in {payload!r}')\n",
                "Detection",
            );
            let refusal = cast_payload_bytes_into_read_target(
                "detections",
                &PyBytes::new(python, b"\x00\x01"),
                &target,
                None,
            )
            .unwrap_err();
            assert!(
                refusal.is_instance_of::<PyValueError>(python),
                "got: {refusal}"
            );
            assert!(
                refusal.to_string().contains("no detection in"),
                "got: {refusal}"
            );
        });
    }

    /// A target with no `from_bytes` — a plain dataclass, a bare TypedDict, a
    /// string — cannot build anything from bytes, and CPython's own
    /// `AttributeError` names neither the port nor `into=`.
    #[test]
    fn a_target_without_from_bytes_is_refused_by_port_name() {
        Python::initialize();
        Python::attach(|python| {
            let plain_dataclass = read_target_from_source(
                python,
                "from dataclasses import dataclass\n@dataclass\nclass Detection:\n    label: str\n",
                "Detection",
            );
            let not_a_class = "Detection".into_pyobject(python).unwrap().into_any();
            for target in [plain_dataclass, not_a_class] {
                let refusal = cast_payload_bytes_into_read_target(
                    "detections",
                    &PyBytes::new(python, b"cat"),
                    &target,
                    None,
                )
                .unwrap_err();
                assert!(
                    refusal.is_instance_of::<PyTypeError>(python),
                    "got: {refusal}"
                );
                let refusal_text = refusal.to_string();
                assert!(
                    refusal_text.contains("\"detections\"")
                        && refusal_text.contains("into=")
                        && refusal_text.contains("from_bytes"),
                    "got: {refusal_text}"
                );
            }
        });
    }

    /// `containers` lists, each holding the next, the innermost empty.
    fn lists_nested_containers_deep(python: Python<'_>, containers: usize) -> Bound<'_, PyAny> {
        let mut nested = PyList::empty(python).into_any();
        for _ in 1..containers {
            nested = PyList::new(python, [nested]).unwrap().into_any();
        }
        nested
    }

    fn assert_refused_as_nesting_past_the_maximum(refusal: &PyErr, python: Python<'_>) {
        assert!(
            refusal.is_instance_of::<PyValueError>(python),
            "got: {refusal}"
        );
        let refusal_text = refusal.value(python).to_string();
        assert!(
            refusal_text.contains(&format!(
                "containers nest more than {MAXIMUM_NESTED_CONTAINER_DEPTH} deep"
            )) && refusal_text.contains("holds itself")
                && refusal_text.contains("Break the cycle"),
            "got: {refusal_text}"
        );
    }

    /// A container holding itself would recurse the conversion off its stack
    /// and take the process with it; every caller gets a refusal instead.
    #[test]
    fn a_container_that_holds_itself_is_refused_rather_than_overflowing_the_stack() {
        Python::initialize();
        Python::attach(|python| {
            let config_holding_itself = PyDict::new(python);
            config_holding_itself.set_item("title", "loop").unwrap();
            config_holding_itself
                .set_item("itself", &config_holding_itself)
                .unwrap();
            let refusal = python_object_to_json_value(config_holding_itself.as_any()).unwrap_err();
            assert_refused_as_nesting_past_the_maximum(&refusal, python);

            let list_holding_itself = PyList::empty(python);
            list_holding_itself.append(&list_holding_itself).unwrap();
            let config_holding_the_list = PyDict::new(python);
            config_holding_the_list
                .set_item("items", &list_holding_itself)
                .unwrap();
            let refusal =
                python_object_to_json_value(config_holding_the_list.as_any()).unwrap_err();
            assert_refused_as_nesting_past_the_maximum(&refusal, python);
        });
    }

    /// The bound counts every container, the outermost included: exactly the
    /// maximum converts and one more is refused.
    #[test]
    fn containers_nested_exactly_the_maximum_deep_convert_and_one_more_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let nested_the_maximum =
                lists_nested_containers_deep(python, MAXIMUM_NESTED_CONTAINER_DEPTH);
            python_object_to_json_value(&nested_the_maximum).unwrap();
            let nested_one_more =
                lists_nested_containers_deep(python, MAXIMUM_NESTED_CONTAINER_DEPTH + 1);
            let refusal = python_object_to_json_value(&nested_one_more).unwrap_err();
            assert_refused_as_nesting_past_the_maximum(&refusal, python);
        });
    }

    /// NaN and infinity have no JSON spelling; config and a loaded graph carry
    /// them as null rather than refusing them.
    #[test]
    fn nan_and_infinity_convert_to_json_null() {
        Python::initialize();
        Python::attach(|python| {
            for not_a_number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let converted =
                    python_object_to_json_value(PyFloat::new(python, not_a_number).as_any())
                        .unwrap();
                assert_eq!(converted, serde_json::Value::Null, "{not_a_number}");
            }
        });
    }

    #[test]
    fn bytes_are_refused_as_data_json_cannot_carry_whoever_converts_them() {
        Python::initialize();
        Python::attach(|python| {
            let refusal =
                python_object_to_json_value(PyBytes::new(python, b"fit").as_any()).unwrap_err();
            assert!(
                refusal.is_instance_of::<PyTypeError>(python),
                "got: {refusal}"
            );
            let refusal_text = refusal.value(python).to_string();
            assert!(
                refusal_text.starts_with("the value must survive a JSON round trip: ")
                    && refusal_text.ends_with("carry `bytes` as a `str` or a list of ints"),
                "got: {refusal_text}"
            );
        });
    }

    #[test]
    fn nesting_far_past_the_maximum_is_refused_rather_than_overflowing_the_stack() {
        Python::initialize();
        Python::attach(|python| {
            let nested_far_past_the_maximum = lists_nested_containers_deep(python, 100_000);
            let refusal = python_object_to_json_value(&nested_far_past_the_maximum).unwrap_err();
            assert_refused_as_nesting_past_the_maximum(&refusal, python);
        });
    }

    /// msgpack `fixarray` markers: one element, and none.
    const MSGPACK_ONE_ELEMENT_ARRAY_MARKER: u8 = 0x91;
    const MSGPACK_EMPTY_ARRAY_MARKER: u8 = 0x90;

    /// `containers` msgpack arrays, each holding the next, the innermost
    /// empty — written byte by byte, because rmpv's own encoder recurses.
    fn msgpack_arrays_nested_containers_deep(containers: usize) -> Vec<u8> {
        let mut encoded = vec![MSGPACK_ONE_ELEMENT_ARRAY_MARKER; containers - 1];
        encoded.push(MSGPACK_EMPTY_ARRAY_MARKER);
        encoded
    }

    /// A map — `{"nested": …}` — whose containers, itself the outermost, nest
    /// `containers` deep.
    fn msgpack_map_nesting_containers_deep(containers: usize) -> Vec<u8> {
        const MSGPACK_ONE_ENTRY_MAP_MARKER: u8 = 0x81;
        const MSGPACK_SIX_BYTE_STRING_MARKER: u8 = 0xa6;
        let mut encoded = vec![MSGPACK_ONE_ENTRY_MAP_MARKER, MSGPACK_SIX_BYTE_STRING_MARKER];
        encoded.extend_from_slice(b"nested");
        encoded.extend(msgpack_arrays_nested_containers_deep(containers - 1));
        encoded
    }

    fn assert_decode_refused_as_nesting_past_the_maximum(refusal: &PyErr, python: Python<'_>) {
        assert!(
            refusal.is_instance_of::<PyValueError>(python),
            "got: {refusal}"
        );
        let refusal_text = refusal.value(python).to_string();
        assert!(
            refusal_text.contains(&format!(
                "containers nest more than {MAXIMUM_NESTED_CONTAINER_DEPTH} deep"
            )) && refusal_text.contains(&format!(
                "Have its producer nest the data at most {MAXIMUM_NESTED_CONTAINER_DEPTH} \
                 containers deep"
            )),
            "got: {refusal_text}"
        );
    }

    /// Decode counts containers the way the JSON conversion does, the outermost
    /// included: exactly the maximum decodes — holding the leaf rmpv spends
    /// most depth on — and one more is refused by name, as is a value deep
    /// enough to exhaust rmpv's own budget.
    #[test]
    fn containers_nested_exactly_the_maximum_deep_decode_and_one_more_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            const MSGPACK_ONE_BYTE_STRING_MARKER: u8 = 0xa1;
            let mut nested_the_maximum_around_a_string =
                vec![MSGPACK_ONE_ELEMENT_ARRAY_MARKER; MAXIMUM_NESTED_CONTAINER_DEPTH];
            nested_the_maximum_around_a_string.extend([MSGPACK_ONE_BYTE_STRING_MARKER, b'x']);
            let decoded =
                decode_msgpack_to_python_object(python, &nested_the_maximum_around_a_string)
                    .unwrap();
            assert!(decoded.is_instance_of::<PyList>());

            let refusal = decode_msgpack_to_python_object(
                python,
                &msgpack_arrays_nested_containers_deep(MAXIMUM_NESTED_CONTAINER_DEPTH + 1),
            )
            .unwrap_err();
            assert_decode_refused_as_nesting_past_the_maximum(&refusal, python);

            let refusal = decode_msgpack_to_python_object(
                python,
                &msgpack_arrays_nested_containers_deep(100_000),
            )
            .unwrap_err();
            assert_decode_refused_as_nesting_past_the_maximum(&refusal, python);
        });
    }

    /// A map counts as the outermost container: one nested to the maximum
    /// decodes and one more is refused.
    #[test]
    fn a_map_nested_to_the_maximum_decodes_and_one_more_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let decoded = decode_msgpack_to_python_object(
                python,
                &msgpack_map_nesting_containers_deep(MAXIMUM_NESTED_CONTAINER_DEPTH),
            )
            .expect("a map nested to the maximum decodes");
            assert!(decoded.is_instance_of::<PyDict>());

            let refusal = decode_msgpack_to_python_object(
                python,
                &msgpack_map_nesting_containers_deep(MAXIMUM_NESTED_CONTAINER_DEPTH + 1),
            )
            .unwrap_err();
            assert_decode_refused_as_nesting_past_the_maximum(&refusal, python);
        });
    }
}
