// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bags across the Python boundary.
//!
//! A bag is a self-describing msgpack value, so the conversion is total in both
//! directions and needs no schema: encoding walks ordinary Python data, decoding
//! rebuilds ordinary Python data. Pixels never travel this way — a frame crosses
//! as a handle, and the payload here is the named map describing it.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use rmpv::Value;

/// Encode a Python object to the msgpack bytes the wire carries.
pub(crate) fn encode_python_object_to_msgpack(value: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    let mut encoded = Vec::new();
    rmpv::encode::write_value(&mut encoded, &python_object_to_msgpack_value(value)?)
        .map_err(|encode_failure| PyValueError::new_err(encode_failure.to_string()))?;
    Ok(encoded)
}

/// Decode msgpack bytes from the wire into ordinary Python data.
pub(crate) fn decode_msgpack_to_python_object<'py>(
    python: Python<'py>,
    encoded: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let value = rmpv::decode::read_value(&mut &encoded[..])
        .map_err(|decode_failure| PyValueError::new_err(decode_failure.to_string()))?;
    msgpack_value_to_python_object(python, &value)
}

/// Convert a processor's JSON configuration into the keyword arguments its
/// class is constructed with.
///
/// Routed through the same msgpack value tree the data plane uses rather than
/// growing a second converter: the engine stores configuration as JSON, and one
/// conversion is one set of edge cases.
pub(crate) fn json_value_to_python_object<'py>(
    python: Python<'py>,
    configuration: &serde_json::Value,
) -> PyResult<Bound<'py, PyAny>> {
    let value = rmpv::ext::to_value(configuration)
        .map_err(|convert_failure| PyValueError::new_err(convert_failure.to_string()))?;
    msgpack_value_to_python_object(python, &value)
}

/// Convert a processor's configuration from Python into the JSON the graph
/// node stores.
///
/// Configuration travels on the node rather than in a closure, so it has to
/// survive a JSON round trip — which is also what keeps it inspectable in
/// `streamlib graph`.
pub(crate) fn python_object_to_json_value(
    configuration: &Bound<'_, PyAny>,
) -> PyResult<serde_json::Value> {
    let value = python_object_to_msgpack_value(configuration)?;
    rmpv::ext::from_value(value).map_err(|convert_failure| {
        PyTypeError::new_err(format!(
            "config must survive a JSON round trip, because the engine stores it on the graph \
             node: {convert_failure}"
        ))
    })
}

fn python_object_to_msgpack_value(value: &Bound<'_, PyAny>) -> PyResult<Value> {
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
        let mut entries = Vec::with_capacity(mapping.len());
        for (key, entry) in mapping.iter() {
            entries.push((
                python_object_to_msgpack_value(&key)?,
                python_object_to_msgpack_value(&entry)?,
            ));
        }
        return Ok(Value::Map(entries));
    }
    if let Ok(sequence) = value.cast::<PyList>() {
        return sequence_to_msgpack_array(sequence.iter());
    }
    if let Ok(sequence) = value.cast::<PyTuple>() {
        return sequence_to_msgpack_array(sequence.iter());
    }
    Err(PyTypeError::new_err(format!(
        "cannot put a {} on a link: a bag is built from dict, list, tuple, str, bytes, int, \
         float, bool and None. A GPU frame is not copied into Python — it travels as a handle.",
        value.get_type().name()?
    )))
}

fn sequence_to_msgpack_array<'py>(
    items: impl Iterator<Item = Bound<'py, PyAny>>,
) -> PyResult<Value> {
    items
        .map(|item| python_object_to_msgpack_value(&item))
        .collect::<PyResult<Vec<Value>>>()
        .map(Value::Array)
}

fn msgpack_value_to_python_object<'py>(
    python: Python<'py>,
    value: &Value,
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
            let converted = items
                .iter()
                .map(|item| msgpack_value_to_python_object(python, item))
                .collect::<PyResult<Vec<_>>>()?;
            PyList::new(python, converted)?.into_any()
        }
        Value::Map(entries) => {
            let mapping = PyDict::new(python);
            for (key, entry) in entries {
                mapping.set_item(
                    msgpack_value_to_python_object(python, key)?,
                    msgpack_value_to_python_object(python, entry)?,
                )?;
            }
            mapping.into_any()
        }
        Value::Ext(type_tag, bytes) => {
            // Preserved rather than dropped: an extension type is somebody
            // else's payload, and a processor that only forwards a bag must be
            // able to hand it on unchanged.
            let extension = PyDict::new(python);
            extension.set_item("__msgpack_ext_type__", type_tag)?;
            extension.set_item("__msgpack_ext_data__", PyBytes::new(python, bytes))?;
            extension.into_any()
        }
    };
    Ok(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value shape a bag can carry survives Python → msgpack → Python
    /// with its type and value intact.
    #[test]
    fn ordinary_python_data_round_trips_through_the_wire() {
        Python::initialize();
        Python::attach(|python| {
            let source = pyo3::types::PyDict::new(python);
            source.set_item("nothing", python.None()).unwrap();
            source.set_item("flag", true).unwrap();
            source.set_item("count", -42i64).unwrap();
            source.set_item("huge", u64::MAX).unwrap();
            source.set_item("ratio", 1.5f64).unwrap();
            source.set_item("label", "カメラ 🎥").unwrap();
            source
                .set_item("payload", PyBytes::new(python, &[0u8, 255, 7]))
                .unwrap();
            source.set_item("items", vec![1i64, 2, 3]).unwrap();
            let nested = pyo3::types::PyDict::new(python);
            nested.set_item("inner", "value").unwrap();
            source.set_item("nested", &nested).unwrap();

            let encoded = encode_python_object_to_msgpack(source.as_any()).unwrap();
            let decoded = decode_msgpack_to_python_object(python, &encoded).unwrap();

            assert!(decoded.eq(&source).unwrap(), "round trip changed the bag");
        });
    }

    /// `bool` is a subclass of `int` in Python, so a naive integer-first check
    /// silently turns `True` into `1`.
    #[test]
    fn booleans_do_not_decay_into_integers() {
        Python::initialize();
        Python::attach(|python| {
            let encoded = encode_python_object_to_msgpack(
                &true.into_pyobject(python).unwrap().to_owned().into_any(),
            )
            .unwrap();
            let decoded = decode_msgpack_to_python_object(python, &encoded).unwrap();
            assert!(decoded.is_instance_of::<PyBool>());
        });
    }

    /// A tuple is a reasonable thing for an author to write, and msgpack has no
    /// tuple — it arrives back as a list rather than being refused.
    #[test]
    fn a_tuple_is_accepted_and_arrives_as_a_list() {
        Python::initialize();
        Python::attach(|python| {
            let source = PyTuple::new(python, [1i64, 2, 3]).unwrap();
            let encoded = encode_python_object_to_msgpack(source.as_any()).unwrap();
            let decoded = decode_msgpack_to_python_object(python, &encoded).unwrap();
            assert!(decoded.is_instance_of::<PyList>());
            assert_eq!(decoded.len().unwrap(), 3);
        });
    }

    /// The refusal names what a bag may hold — an author who reaches for a
    /// numpy array or a socket should learn the rule from the error.
    #[test]
    fn an_unencodable_object_is_refused_with_the_rule() {
        Python::initialize();
        Python::attach(|python| {
            let unencodable = python
                .import("builtins")
                .unwrap()
                .getattr("object")
                .unwrap()
                .call0()
                .unwrap();
            let failure = encode_python_object_to_msgpack(&unencodable).unwrap_err();
            assert!(
                failure.to_string().contains("a bag is built from"),
                "the refusal must state what is allowed, got: {failure}"
            );
        });
    }
}
