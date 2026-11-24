// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Python data across the boundary: msgpack decoded into ordinary Python data,
//! config and graph JSON both ways, and the GPU capability a read offers the family
//! type it builds. A message's payload otherwise crosses as the bytes it is;
//! nothing here encodes one.

use std::cell::RefCell;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use rmpv::Value;
use streamlib::sdk::iceoryx2::RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES;

use crate::python_node_context::PythonGpuContextLimitedAccess;

/// Call `family_type` with `keyword_arguments` while this read offers its GPU
/// capability, as a family type a read builds is constructed.
pub(crate) fn family_type_built_offering_gpu_access<'py>(
    family_type: &Bound<'py, PyAny>,
    keyword_arguments: &Bound<'py, PyDict>,
    offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
) -> PyResult<Bound<'py, PyAny>> {
    let _offer = TypedReadGpuAccessOffer::open(offered_gpu_limited_access);
    family_type.call((), Some(keyword_arguments))
}

// =============================================================================
// What a typed read offers the type it is constructing
// =============================================================================

thread_local! {
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

/// The GPU capability of the read currently building a family type, an
/// `ImageFrame` or an Array frame's GPU attribute, or `None` when no read is.
///
/// The same capability as `ctx.gpu_limited_access`, offered so the type can do
/// per-frame work as it is built that needs the engine — claiming the frame's
/// surface against producer reuse is what `ClaimedSurfacePixelAccess` does
/// with it.
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
/// Routed through the msgpack value tree the msgpack decode uses rather than
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

/// Encode `record`, a dict, as a Records message's msgpack payload, refused
/// when it is not a dict, lacks one of `declared_names`, or encodes past
/// [`RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES`], naming the port.
pub(crate) fn records_message_payload_of(
    port_name: &str,
    record: &Bound<'_, PyAny>,
    declared_names: &[String],
) -> PyResult<Vec<u8>> {
    let mapping = record.cast::<PyDict>().map_err(|_| {
        PyTypeError::new_err(format!(
            "a write on Records output port {port_name:?} takes a dict; got a {}",
            python_type_name_for_error_message(record, "value")
        ))
    })?;
    let missing_names: Vec<&String> = declared_names
        .iter()
        .filter(|name| !mapping.contains(name.as_str()).unwrap_or(false))
        .collect();
    if !missing_names.is_empty() {
        return Err(PyValueError::new_err(format!(
            "a write on Records output port {port_name:?} is missing the declared keys \
             {missing_names:?}; every write carries each name the port declares"
        )));
    }
    let value = named_map_to_msgpack_value(mapping, ContainersEnclosingAValue::OUTERMOST).map_err(
        |refusal| {
            PyErr::from_type(
                refusal.get_type(record.py()),
                format!(
                    "a write on Records output port {port_name:?}: {}",
                    refusal.value(record.py())
                ),
            )
        },
    )?;
    let mut encoded = Vec::new();
    rmpv::encode::write_value(&mut encoded, &value)
        .map_err(|encode_failure| PyValueError::new_err(encode_failure.to_string()))?;
    if encoded.len() > RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES {
        return Err(PyValueError::new_err(format!(
            "a write on Records output port {port_name:?} encodes to {} bytes, past the \
             {RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES} a Records message may take",
            encoded.len()
        )));
    }
    Ok(encoded)
}

/// Decode a Records message's msgpack payload into the dict it carries.
pub(crate) fn records_message_dict_of<'py>(
    python: Python<'py>,
    port_name: &str,
    payload: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let mut remaining = payload;
    let value = rmpv::decode::read_value(&mut remaining).map_err(|decode_failure| {
        PyValueError::new_err(format!(
            "a Records message on {port_name:?} does not decode: {decode_failure}"
        ))
    })?;
    let Value::Map(entries) = &value else {
        return Err(PyValueError::new_err(format!(
            "a Records message on {port_name:?} carries no dict"
        )));
    };
    if let Some((key, _)) = entries.iter().find(|(key, _)| !key.is_str()) {
        return Err(PyValueError::new_err(format!(
            "a Records message on {port_name:?} carries key {key}, and a record's keys are \
             strings"
        )));
    }
    if !remaining.is_empty() {
        return Err(PyValueError::new_err(format!(
            "a Records message on {port_name:?} carries {} bytes past its one msgpack map",
            remaining.len()
        )));
    }
    msgpack_value_to_python_object(python, &value, ContainersEnclosingAValue::OUTERMOST)
}

/// The most containers one value may nest, the outermost counted, as it
/// crosses the Python boundary in either direction. The walk recurses once per
/// container, so the bound keeps it inside the thread's stack and turns a
/// container that holds itself into a refusal.
const MAXIMUM_NESTED_CONTAINER_DEPTH: usize = 128;

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

/// The keys a decoded msgpack extension value is carried under, so a message
/// holding somebody else's extension type reads as data a reader can look at.
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
    use super::*;

    fn msgpack_map_of(entries: Vec<(&str, Value)>) -> Value {
        Value::Map(
            entries
                .into_iter()
                .map(|(key, entry)| (Value::from(key), entry))
                .collect(),
        )
    }

    #[test]
    fn a_records_message_with_bytes_past_its_map_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let mut payload = Vec::new();
            rmpv::encode::write_value(&mut payload, &msgpack_map_of(vec![("a", Value::from(1))]))
                .unwrap();
            payload.push(0xc0);
            let refusal = records_message_dict_of(python, "events", &payload)
                .expect_err("a byte past the map is no part of the record");
            assert!(refusal.to_string().contains("1 bytes past"), "{refusal}");

            let mut integer_keyed = Vec::new();
            rmpv::encode::write_value(
                &mut integer_keyed,
                &Value::Map(vec![(Value::from(7), Value::from(1))]),
            )
            .unwrap();
            let refusal = records_message_dict_of(python, "events", &integer_keyed)
                .expect_err("a record's keys are strings");
            assert!(refusal.to_string().contains("key 7"), "{refusal}");
        });
    }

    /// Every msgpack value shape converts into the Python type a reader of a
    /// message expects: `bin` as `bytes`, a boolean as `bool` rather
    /// than an `int`, an array as a list, and an extension value as its type
    /// and data.
    #[test]
    fn every_msgpack_value_shape_converts_into_ordinary_python_data() {
        Python::initialize();
        Python::attach(|python| {
            let published = msgpack_map_of(vec![
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
            ]);

            let decoded = msgpack_value_to_python_object(
                python,
                &published,
                ContainersEnclosingAValue::OUTERMOST,
            )
            .unwrap();

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
}
