// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bags across the Python boundary.
//!
//! A bag is a self-describing msgpack value, so the conversion is total in both
//! directions and needs no schema: encoding walks ordinary Python data, decoding
//! rebuilds ordinary Python data. Pixels never travel this way — a frame crosses
//! as a handle, and the payload here is the named map describing it.

use std::cell::RefCell;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};
use rmpv::Value;

/// Encode a bag to the msgpack bytes the wire carries.
///
/// A bag is a named map — a dict with string keys — because that is what the
/// wire format is, not a convention this layer invented: a Rust processor
/// downstream deserializes the payload into a struct, which needs named fields.
/// A list, a bare scalar, or a dict with non-string keys encodes to msgpack
/// fine and then cannot be read on the other side, so it is refused here rather
/// than published as bytes only Python can decode.
pub(crate) fn encode_bag_to_msgpack(bag: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    let named_map = bag.cast::<PyDict>().map_err(|_| {
        PyTypeError::new_err(format!(
            "a bag is a dict with string keys — the wire carries a named map, and a \
             {} cannot be read as one by a processor in another language. Wrap it: \
             `{{\"value\": …}}`.",
            python_type_name_for_error_message(bag, "value")
        ))
    })?;

    let mut encoded = Vec::new();
    rmpv::encode::write_value(&mut encoded, &named_map_to_msgpack_value(named_map)?)
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

/// Cast or construct a decoded bag into the type an author named with
/// `read(port, into=T)`.
///
/// A TypedDict is a dict at runtime, so the decoded bag already is one and
/// travels back untouched — that is the free cast. Every other target is
/// constructed from the bag's entries as keyword arguments, and whatever the
/// constructor raises is what the author sees: a pydantic `ValidationError`
/// says more about the mismatch than any wrapper here could.
///
/// `offered_gpu_limited_access` is what the constructing type may reach for
/// through [`gpu_limited_access_of_the_typed_read_in_progress`] — offered for
/// the construction and withdrawn the moment it returns. Nothing here knows
/// which types want it or what they do with it.
pub(crate) fn cast_decoded_bag_into_read_target<'py>(
    port_name: &str,
    decoded_bag: Bound<'py, PyAny>,
    read_target_type: &Bound<'py, PyAny>,
    offered_gpu_limited_access: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let named_map = decoded_bag.cast::<PyDict>().map_err(|_| {
        PyTypeError::new_err(format!(
            "the bag on input port {port_name:?} decoded as a {}, and `into=` builds its target \
             from a named map's entries. Read it without `into=` to see what arrived.",
            python_type_name_for_error_message(&decoded_bag, "value")
        ))
    })?;
    if read_target_is_a_typed_dict(read_target_type)? {
        // Nothing is constructed, so there is no constructor to offer
        // anything to: the bag arrives as itself.
        return Ok(decoded_bag);
    }
    let _offer = TypedReadGpuAccessOffer::open(offered_gpu_limited_access);
    read_target_type
        .call((), Some(named_map))
        .map_err(|construction_failure| {
            if read_target_type.is_callable() {
                return construction_failure;
            }
            PyTypeError::new_err(format!(
                "`into=` on input port {port_name:?} was handed a {}, which cannot be called to \
                 build anything — name a TypedDict, a dataclass, or a model class.",
                python_type_name_for_error_message(read_target_type, "value")
            ))
        })
}

// =============================================================================
// What a typed read offers the type it is constructing
// =============================================================================

thread_local! {
    /// The GPU capability the typed read running on this thread is offering,
    /// and only while it is constructing.
    ///
    /// Thread-local rather than passed: `read(port, into=T)` builds `T` by
    /// calling it with the bag's entries, which is the whole contract — an
    /// argument the engine added would be one every target had to accept, and
    /// most targets are ordinary dataclasses that want nothing to do with a
    /// GPU. A processor's callbacks run on its own thread, and construction
    /// cannot yield to another read on that thread, so the window is exactly
    /// one constructor call deep.
    static GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ: RefCell<Option<Py<PyAny>>> =
        const { RefCell::new(None) };
}

/// Opens the offer for one construction and withdraws it on drop, whether the
/// constructor returned or raised.
struct TypedReadGpuAccessOffer {
    /// Restored rather than cleared: a target whose constructor reads its own
    /// input ports would otherwise leave the outer read's offer withdrawn.
    offer_this_read_displaced: Option<Py<PyAny>>,
}

impl TypedReadGpuAccessOffer {
    fn open(offered_gpu_limited_access: Option<&Bound<'_, PyAny>>) -> Self {
        let offered = offered_gpu_limited_access.map(|capability| capability.clone().unbind());
        Self {
            offer_this_read_displaced: GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ
                .with(|offer| offer.replace(offered)),
        }
    }
}

impl Drop for TypedReadGpuAccessOffer {
    fn drop(&mut self) {
        let restored = self.offer_this_read_displaced.take();
        GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ.with(|offer| {
            *offer.borrow_mut() = restored;
        });
    }
}

/// The GPU capability of the `read(port, into=T)` currently constructing an
/// object, or `None` when nothing is being read into a type.
///
/// The same capability as `ctx.gpu_limited_access`, offered so a type can do
/// per-frame work at construction that needs the engine — claiming the frame's
/// surface against producer reuse is what the shipped `VideoFrame` does with
/// it. Any class reachable through `into=` may call this; there is no
/// registration, no marker and no privileged type.
#[pyfunction]
pub(crate) fn gpu_limited_access_of_the_typed_read_in_progress(
    python: Python<'_>,
) -> Option<Py<PyAny>> {
    GPU_LIMITED_ACCESS_OFFERED_TO_THE_TYPED_READ.with(|offer| {
        offer
            .borrow()
            .as_ref()
            .map(|capability| capability.clone_ref(python))
    })
}

/// Whether `read_target_type` is a TypedDict class.
///
/// Structural rather than `typing.is_typeddict`, which knows only the stdlib's
/// TypedDict and answers `False` for one built from `typing_extensions` — the
/// spelling a package supporting several interpreter versions uses. Every
/// TypedDict class, from either module, is a `dict` subclass carrying
/// `__required_keys__`.
fn read_target_is_a_typed_dict(read_target_type: &Bound<'_, PyAny>) -> PyResult<bool> {
    let Ok(target_class) = read_target_type.cast::<PyType>() else {
        return Ok(false);
    };
    // Interned: this is the per-frame free-cast path, and a plain `&str` here
    // allocates a Python string per read.
    Ok(target_class.is_subclass_of::<PyDict>()?
        && target_class.hasattr(intern!(target_class.py(), "__required_keys__"))?)
}

/// The type name to name in a refusal, or `unknown_type_placeholder` when even
/// that cannot be read.
fn python_type_name_for_error_message(
    value: &Bound<'_, PyAny>,
    unknown_type_placeholder: &str,
) -> String {
    value.get_type().name().map_or_else(
        |_| unknown_type_placeholder.to_string(),
        |type_name| type_name.to_string(),
    )
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
        return match msgpack_extension_from_python_mapping(mapping)? {
            Some(extension) => Ok(extension),
            None => named_map_to_msgpack_value(mapping),
        };
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

/// The keys a decoded msgpack extension value is carried under.
///
/// An extension type is somebody else's payload, so decoding renders it as data
/// a processor can look at and encoding puts it back exactly as it was — a
/// passthrough processor must not rewrite what it only forwards. This holds for
/// a nested value; a bag is a named map, so a *top-level* extension is not a bag
/// and never re-encodes as one.
const EXTENSION_TYPE_KEY: &str = "__msgpack_ext_type__";
const EXTENSION_DATA_KEY: &str = "__msgpack_ext_data__";

/// Recognize the shape [`msgpack_value_to_python_object`] decodes an extension
/// value into, so it re-encodes as one.
fn msgpack_extension_from_python_mapping(mapping: &Bound<'_, PyDict>) -> PyResult<Option<Value>> {
    if mapping.len() != 2 {
        return Ok(None);
    }
    let (Some(type_tag), Some(data)) = (
        mapping.get_item(EXTENSION_TYPE_KEY)?,
        mapping.get_item(EXTENSION_DATA_KEY)?,
    ) else {
        return Ok(None);
    };
    let (Ok(type_tag), Ok(data)) = (type_tag.extract::<i8>(), data.cast::<PyBytes>()) else {
        return Ok(None);
    };
    Ok(Some(Value::Ext(type_tag, data.as_bytes().to_vec())))
}

/// Encode a dict, requiring string keys at every level.
///
/// msgpack maps allow any key type, but a named map is what every other
/// language on the wire deserializes into a struct — an int-keyed map would
/// encode and then fail to read there.
fn named_map_to_msgpack_value(mapping: &Bound<'_, PyDict>) -> PyResult<Value> {
    let mut entries = Vec::with_capacity(mapping.len());
    for (key, entry) in mapping.iter() {
        let key = key.cast::<PyString>().map_err(|_| {
            PyTypeError::new_err(format!(
                "bag keys must be strings — the wire carries a named map; got a {} key",
                python_type_name_for_error_message(&key, "non-string")
            ))
        })?;
        entries.push((
            Value::from(key.to_cow()?.into_owned()),
            python_object_to_msgpack_value(&entry)?,
        ));
    }
    Ok(Value::Map(entries))
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

            let encoded = encode_bag_to_msgpack(source.as_any()).unwrap();
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
            let source = PyDict::new(python);
            source.set_item("flag", true).unwrap();
            let encoded = encode_bag_to_msgpack(source.as_any()).unwrap();
            let decoded = decode_msgpack_to_python_object(python, &encoded).unwrap();
            assert!(decoded.get_item("flag").unwrap().is_instance_of::<PyBool>());
        });
    }

    /// A tuple is a reasonable thing for an author to write, and msgpack has no
    /// tuple — it arrives back as a list rather than being refused.
    #[test]
    fn a_tuple_is_accepted_and_arrives_as_a_list() {
        Python::initialize();
        Python::attach(|python| {
            let source = PyDict::new(python);
            source
                .set_item("items", PyTuple::new(python, [1i64, 2, 3]).unwrap())
                .unwrap();
            let encoded = encode_bag_to_msgpack(source.as_any()).unwrap();
            let decoded = decode_msgpack_to_python_object(python, &encoded).unwrap();
            let items = decoded.get_item("items").unwrap();
            assert!(items.is_instance_of::<PyList>());
            assert_eq!(items.len().unwrap(), 3);
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
            let source = PyDict::new(python);
            source.set_item("payload", unencodable).unwrap();
            let failure = encode_bag_to_msgpack(source.as_any()).unwrap_err();
            assert!(
                failure.to_string().contains("a bag is built from"),
                "the refusal must state what is allowed, got: {failure}"
            );
        });
    }

    /// A bag that is not a named map encodes to valid msgpack and is then
    /// unreadable by a processor in another language, so it is refused at the
    /// boundary rather than published.
    #[test]
    fn a_bag_that_is_not_a_named_map_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let list_bag = PyList::new(python, [1i64, 2, 3]).unwrap();
            let failure = encode_bag_to_msgpack(list_bag.as_any()).unwrap_err();
            assert!(
                failure
                    .to_string()
                    .contains("a bag is a dict with string keys"),
                "got: {failure}"
            );

            let int_keyed = PyDict::new(python);
            int_keyed.set_item(1i64, "value").unwrap();
            let failure = encode_bag_to_msgpack(int_keyed.as_any()).unwrap_err();
            assert!(
                failure.to_string().contains("bag keys must be strings"),
                "got: {failure}"
            );

            // Nested too — a named map all the way down, not just at the top.
            let nested_int_keyed = PyDict::new(python);
            nested_int_keyed.set_item("inner", &int_keyed).unwrap();
            assert!(encode_bag_to_msgpack(nested_int_keyed.as_any()).is_err());
        });
    }

    /// Build a target class the way an author's module would, so the runtime
    /// carries whatever CPython actually assigns rather than attributes a test
    /// set by hand.
    fn read_target_from_source<'py>(
        python: Python<'py>,
        source: &str,
        class_name: &str,
    ) -> Bound<'py, PyAny> {
        let namespace = PyDict::new(python);
        python
            .run(
                &std::ffi::CString::new(source).unwrap(),
                Some(&namespace),
                None,
            )
            .unwrap();
        namespace.get_item(class_name).unwrap().unwrap()
    }

    fn bag_with_one_name<'py>(python: Python<'py>, name: &str) -> Bound<'py, PyAny> {
        let bag = PyDict::new(python);
        bag.set_item("name", name).unwrap();
        bag.into_any()
    }

    /// The free cast: a TypedDict is a dict at runtime, so the bag arrives as
    /// itself — the same object, not a copy, and nothing validated. A bag
    /// missing a declared key still reads, which is what "free" means.
    #[test]
    fn a_typed_dict_target_hands_back_the_bag_without_constructing() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                "from typing import TypedDict\nclass Detection(TypedDict):\n    name: str\n    \
                 score: float\n",
                "Detection",
            );
            let bag = bag_with_one_name(python, "cat");
            let read = cast_decoded_bag_into_read_target("detections", bag.clone(), &target, None)
                .expect("a TypedDict target validates nothing, so a partial bag must read");
            assert!(read.is(&bag), "the free cast must not copy the bag");
        });
    }

    /// The free cast has to survive a TypedDict `typing.is_typeddict` does not
    /// recognize — a `typing_extensions.TypedDict` is one on every interpreter
    /// this wheel supports, and the predicate answers `False` for it.
    ///
    /// Spelled here as the structure every TypedDict class shares rather than
    /// by importing typing_extensions: the interpreter these tests link is the
    /// system one, which carries no third-party packages.
    #[test]
    fn a_typed_dict_the_stdlib_predicate_misses_still_casts_for_free() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                "class Detection(dict):\n    __required_keys__ = frozenset({'name'})\n    \
                 __optional_keys__ = frozenset()\n",
                "Detection",
            );
            assert!(
                !python
                    .import("typing")
                    .unwrap()
                    .call_method1("is_typeddict", (&target,))
                    .unwrap()
                    .is_truthy()
                    .unwrap(),
                "this target must be one the stdlib predicate rejects, or it proves nothing"
            );

            let bag = bag_with_one_name(python, "cat");
            let read =
                cast_decoded_bag_into_read_target("detections", bag.clone(), &target, None).unwrap();

            assert!(read.is(&bag), "the free cast must not copy the bag");
        });
    }

    /// A dataclass is the constructing half of the dial: a good bag builds an
    /// instance, and construction is the validation.
    #[test]
    fn a_dataclass_target_is_constructed_from_the_bag() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                "from dataclasses import dataclass\n@dataclass\nclass Detection:\n    name: str\n",
                "Detection",
            );
            let read = cast_decoded_bag_into_read_target(
                "detections",
                bag_with_one_name(python, "cat"),
                &target,
                None,
            )
            .unwrap();
            assert!(read.is_instance(&target).unwrap());
            assert_eq!(
                read.getattr("name").unwrap().extract::<String>().unwrap(),
                "cat"
            );
        });
    }

    /// The dial's whole point: a bag that does not fit the declared target
    /// raises at the consuming read rather than travelling on as a mapping.
    #[test]
    fn a_bag_the_target_cannot_be_built_from_raises_at_the_read() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                "from dataclasses import dataclass\n@dataclass\nclass Detection:\n    name: str\n",
                "Detection",
            );
            let bag = PyDict::new(python);
            bag.set_item("label", "cat").unwrap();
            let refusal = cast_decoded_bag_into_read_target("detections", bag.into_any(), &target, None)
                .unwrap_err();
            assert!(
                refusal.to_string().contains("label"),
                "the constructor's own refusal must reach the author: {refusal}"
            );
        });
    }

    /// `into=` builds its target from a named map's entries, so a bag that is
    /// not one has to say so — the alternative is CPython's bare "argument
    /// after ** must be a mapping", which names neither the port nor the bag.
    #[test]
    fn a_bag_that_is_not_a_named_map_is_refused_by_port_name() {
        Python::initialize();
        Python::attach(|python| {
            let target = read_target_from_source(
                python,
                "from dataclasses import dataclass\n@dataclass\nclass Detection:\n    name: str\n",
                "Detection",
            );
            let not_a_bag = PyList::new(python, [1i64, 2, 3]).unwrap().into_any();
            let refusal =
                cast_decoded_bag_into_read_target("detections", not_a_bag, &target, None).unwrap_err();
            assert!(
                refusal.to_string().contains("detections")
                    && refusal.to_string().contains("named map"),
                "got: {refusal}"
            );
        });
    }

    /// A target that is not a class at all cannot build anything, and CPython's
    /// own "not callable" names neither the port nor `into=`.
    #[test]
    fn a_target_that_cannot_be_called_is_refused_by_port_name() {
        Python::initialize();
        Python::attach(|python| {
            let not_a_class = "Detection".into_pyobject(python).unwrap().into_any();
            let refusal = cast_decoded_bag_into_read_target(
                "detections",
                bag_with_one_name(python, "cat"),
                &not_a_class,
                None,
            )
            .unwrap_err();
            assert!(
                refusal.to_string().contains("detections") && refusal.to_string().contains("into="),
                "got: {refusal}"
            );
        });
    }

    /// An extension value survives a processor that only forwards the bag.
    ///
    /// Nothing in the engine emits one today, but a decode that turned it into
    /// an ordinary map would make a passthrough processor silently rewrite
    /// somebody else's payload.
    #[test]
    fn a_msgpack_extension_value_round_trips_unchanged() {
        Python::initialize();
        Python::attach(|python| {
            let original = Value::Map(vec![(
                Value::from("payload"),
                Value::Ext(42, vec![1, 2, 3]),
            )]);
            let mut encoded = Vec::new();
            rmpv::encode::write_value(&mut encoded, &original).unwrap();

            let decoded = decode_msgpack_to_python_object(python, &encoded).unwrap();
            let re_encoded = encode_bag_to_msgpack(&decoded).unwrap();

            assert_eq!(
                rmpv::decode::read_value(&mut &re_encoded[..]).unwrap(),
                original,
                "an Ext value did not survive decode-then-encode"
            );
        });
    }
}
