// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading the `@processor` grammar off a Python class.
//!
//! The `__streamlib_processor_*__` attributes the decorator attaches are the
//! contract between `_processor_declaration.py` and this module; the two move
//! together.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use streamlib::sdk::descriptors::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AudioWindowContract,
    AudioWindowContractDeclaredValues, PortDescriptor, ProcessorClassImportPath,
    ProcessorClassShortName, ProcessorDescriptor, ProcessorRuntime, ProcessorScheduling,
};
use streamlib::sdk::execution::{ExecutionConfig, ProcessExecution, ThreadPriority};

use crate::python_processor_import_path::processor_class_import_path;

/// Everything the engine needs to register and instantiate one Python
/// processor class.
pub(crate) struct PythonProcessorDeclaration {
    pub(crate) descriptor: ProcessorDescriptor,
    pub(crate) execution_config: ExecutionConfig,
}

impl PythonProcessorDeclaration {
    /// Read the decorator's metadata off `processor_class`.
    pub(crate) fn read_from_class(processor_class: &Bound<'_, PyAny>) -> PyResult<Self> {
        let class_short_name = read_class_short_name(processor_class)?;
        let execution_config = read_execution_config(processor_class)?;

        // Identity and `entrypoint` are separate contracts, but one
        // derivation: a second call is a second chance for them to disagree.
        let class_import_path = processor_class_import_path(processor_class)?;

        let mut descriptor = ProcessorDescriptor::new(
            class_short_name,
            ProcessorClassImportPath::new(class_import_path.clone())
                .map_err(|blank| PyValueError::new_err(blank.to_string()))?,
            read_string_attribute(processor_class, "__streamlib_processor_description__")?,
        )
        .with_runtime(ProcessorRuntime::Python)
        .with_entrypoint(class_import_path)
        .with_scheduling(ProcessorScheduling {
            priority: read_thread_priority(processor_class)?,
        });

        descriptor.inputs = read_port_descriptors(processor_class, PortDirection::Input)?;
        descriptor.outputs = read_port_descriptors(processor_class, PortDirection::Output)?;

        Ok(Self {
            descriptor,
            execution_config,
        })
    }
}

/// Whether a Python class carries the decorator's metadata at all.
pub(crate) fn is_declared_processor_class(candidate: &Bound<'_, PyAny>) -> bool {
    candidate.is_instance_of::<pyo3::types::PyType>()
        && candidate
            .hasattr("__streamlib_processor_declared__")
            .unwrap_or(false)
}

/// The class's short name — what an instance's display name defaults to.
///
/// `__name__` is CPython's own short name for the class (`Inner` for a nested
/// `Outer.Inner`), so it needs no string surgery. The import path is the
/// separate `__module__`/`__qualname__` derivation; neither is recovered from
/// the other.
fn read_class_short_name(processor_class: &Bound<'_, PyAny>) -> PyResult<ProcessorClassShortName> {
    let short_name = processor_class.getattr("__name__")?.extract::<String>()?;
    ProcessorClassShortName::new(short_name)
        .map_err(|blank| PyValueError::new_err(blank.to_string()))
}

fn read_execution_config(processor_class: &Bound<'_, PyAny>) -> PyResult<ExecutionConfig> {
    let execution = processor_class
        .getattr("__streamlib_processor_execution__")?
        .cast_into::<PyDict>()
        .map_err(|_| PyTypeError::new_err("__streamlib_processor_execution__ must be a dict"))?;

    let mode = read_dict_string(&execution, "mode")?;
    let execution = match mode.as_str() {
        "reactive" => ProcessExecution::Reactive,
        "manual" => ProcessExecution::Manual,
        "continuous" => ProcessExecution::Continuous {
            interval_ms: execution.get_item("interval_ms")?.map_or(Ok(0), |value| {
                value.extract::<u32>().map_err(|_| {
                    PyTypeError::new_err(
                        "__streamlib_processor_execution__.interval_ms must be an int",
                    )
                })
            })?,
        },
        unknown => {
            return Err(PyTypeError::new_err(format!(
                "unknown execution mode {unknown:?} — the decorator validates this, so a class \
                 reaching here was built by hand rather than by @streamlib.processor"
            )));
        }
    };
    Ok(ExecutionConfig::new(execution))
}

fn read_thread_priority(processor_class: &Bound<'_, PyAny>) -> PyResult<ThreadPriority> {
    let priority = processor_class.getattr("__streamlib_processor_scheduling_priority__")?;
    if priority.is_none() {
        return Ok(ThreadPriority::Normal);
    }
    match priority.extract::<String>()?.as_str() {
        "realtime" => Ok(ThreadPriority::RealTime),
        "high" => Ok(ThreadPriority::High),
        "normal" => Ok(ThreadPriority::Normal),
        unknown => Err(PyTypeError::new_err(format!(
            "unknown scheduling priority {unknown:?}"
        ))),
    }
}

#[derive(Clone, Copy)]
enum PortDirection {
    Input,
    Output,
}

impl PortDirection {
    fn class_attribute(self) -> &'static str {
        match self {
            Self::Input => "__streamlib_processor_input_ports__",
            Self::Output => "__streamlib_processor_output_ports__",
        }
    }
}

fn read_port_descriptors(
    processor_class: &Bound<'_, PyAny>,
    direction: PortDirection,
) -> PyResult<Vec<PortDescriptor>> {
    let attribute = direction.class_attribute();
    let declared = processor_class
        .getattr(attribute)?
        .cast_into::<PyList>()
        .map_err(|_| PyTypeError::new_err(format!("{attribute} must be a list")))?;

    let mut ports = Vec::with_capacity(declared.len());
    for declaration in declared.iter() {
        let declaration = declaration
            .cast_into::<PyDict>()
            .map_err(|_| PyTypeError::new_err(format!("{attribute} must hold dicts")))?;

        let mut port = PortDescriptor::iceoryx2(
            read_dict_string(&declaration, "name")?,
            read_dict_string(&declaration, "description")?,
        );
        if let Some(delivery_profile) = declaration
            .get_item("delivery_profile")?
            .filter(|declared| !declared.is_none())
        {
            port = port.with_delivery_profile(delivery_profile.extract::<String>()?);
        }
        if let Some(audio_window) = declaration
            .get_item("audio_window")?
            .filter(|declared| !declared.is_none())
        {
            if matches!(direction, PortDirection::Output) {
                return Err(PyValueError::new_err(format!(
                    "output port {:?} declares an audio_window — a producer publishes what \
                     it has, and only a consuming input port states the window it needs",
                    port.name
                )));
            }
            let contract = read_audio_window_contract(
                &audio_window,
                &port.name,
                port.delivery_profile.as_deref(),
            )?;
            port = port.with_audio_window_contract(contract);
        }
        ports.push(port);
    }
    Ok(ports)
}

/// Read one `audio_window` declaration off a Python port marker.
///
/// The wheel's own decorator validates first, so this is not the only guard —
/// it is the guard that holds when the marker was built by something other
/// than the decorator, and it renders its refusals from the same shared
/// validator the `#[processor]` grammar uses.
fn read_audio_window_contract(
    audio_window: &Bound<'_, PyAny>,
    port_name: &str,
    delivery_profile: Option<&str>,
) -> PyResult<AudioWindowContract> {
    let declaration = audio_window.cast::<PyDict>().map_err(|_| {
        PyTypeError::new_err(format!(
            "input port {port_name:?}: audio_window must be a dict"
        ))
    })?;

    streamlib::sdk::descriptors::refuse_audio_window_beside_a_skipping_delivery_profile(
        delivery_profile,
    )
    .map_err(|refusal| PyValueError::new_err(format!("input port {port_name:?}: {refusal}")))?;

    let resolved_from = read_audio_window_string_field(declaration, "resolved_from", port_name)?;
    match resolved_from.as_str() {
        "match_device" => Ok(AudioWindowContract::MatchDevice {}),
        "declaration" => {
            let values = AudioWindowContractDeclaredValues {
                sample_rate: read_audio_window_numeric_field(
                    declaration,
                    "sample_rate",
                    port_name,
                )?,
                channels: read_audio_window_channel_count(declaration, port_name)?,
                dtype: read_audio_window_string_field(declaration, "dtype", port_name)?,
                window_size: read_audio_window_numeric_field(
                    declaration,
                    "window_size",
                    port_name,
                )?,
                hop: read_audio_window_numeric_field(declaration, "hop", port_name)?,
            };
            values.refuse_if_unhonourable().map_err(|refusal| {
                PyValueError::new_err(format!("input port {port_name:?}: {refusal}"))
            })?;
            Ok(AudioWindowContract::Declaration(values))
        }
        other => Err(PyValueError::new_err(format!(
            "input port {port_name:?}: audio_window `resolved_from` is {other:?} — expected \
             \"declaration\" or \"match_device\""
        ))),
    }
}

/// Read the `audio_window` channel count an author declared, or `None` where
/// they left it to the source.
///
/// The count is the one value a contract may omit, so an absent key is legal
/// here where every other field's absence is refused by name.
fn read_audio_window_channel_count(
    declaration: &Bound<'_, PyDict>,
    port_name: &str,
) -> PyResult<Option<u32>> {
    let Some(value) = declaration.get_item("channels")? else {
        return Ok(None);
    };
    read_a_channel_count_or_the_source_spelling(&value).map_err(|refusal| {
        PyValueError::new_err(format!(
            "input port {port_name:?}: audio_window field \"channels\" {refusal}"
        ))
    })
}

/// Read a `channels` value written either as a count or as the
/// source-following spelling.
///
/// The one parse both wheel-side readers call — the bridge from an author's
/// declaration and the helper's reading of what the parent wired. The refusal
/// comes back bare so each frames it in its own terms, the way the contract's
/// own validator does: one names a declaration, the other names a wiring.
pub(crate) fn read_a_channel_count_or_the_source_spelling(
    value: &Bound<'_, PyAny>,
) -> Result<Option<u32>, String> {
    if let Ok(spelling) = value.extract::<String>() {
        if spelling == AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE {
            return Ok(None);
        }
        return Err(format!(
            "is {spelling:?} — expected a channel count, or \
             {AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE:?} to carry whatever count the \
             source sends"
        ));
    }

    let declared = value.extract::<i64>().map_err(|_| {
        format!(
            "must be an int, or {AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE:?} to carry \
             whatever count the source sends"
        )
    })?;
    u32::try_from(declared)
        .map(Some)
        .map_err(|_| format!("is {declared} — every numeric field is strictly positive"))
}

/// Read one strictly-positive `audio_window` numeric field, refusing a
/// negative integer by name rather than as an extraction failure.
fn read_audio_window_numeric_field(
    declaration: &Bound<'_, PyDict>,
    key: &str,
    port_name: &str,
) -> PyResult<u32> {
    let value = audio_window_field(declaration, key, port_name)?;
    let declared = value.extract::<i64>().map_err(|_| {
        PyTypeError::new_err(format!(
            "input port {port_name:?}: audio_window field {key:?} must be an int"
        ))
    })?;
    u32::try_from(declared).map_err(|_| {
        PyValueError::new_err(format!(
            "input port {port_name:?}: audio_window field {key:?} is {declared} — every \
             numeric field is strictly positive"
        ))
    })
}

/// Read one `audio_window` string field, naming the port the way every other
/// field of the contract does.
fn read_audio_window_string_field(
    declaration: &Bound<'_, PyDict>,
    key: &str,
    port_name: &str,
) -> PyResult<String> {
    audio_window_field(declaration, key, port_name)?
        .extract::<String>()
        .map_err(|_| {
            PyTypeError::new_err(format!(
                "input port {port_name:?}: audio_window field {key:?} must be a string"
            ))
        })
}

/// One missing-`audio_window`-field refusal, so no field of the contract
/// falls through to a bare `missing key` with no port and no contract named.
fn audio_window_field<'py>(
    declaration: &Bound<'py, PyDict>,
    key: &str,
    port_name: &str,
) -> PyResult<Bound<'py, PyAny>> {
    declaration.get_item(key)?.ok_or_else(|| {
        PyValueError::new_err(format!(
            "input port {port_name:?}: audio_window is missing {key:?} — the contract is \
             all-or-nothing"
        ))
    })
}

fn read_string_attribute(object: &Bound<'_, PyAny>, attribute: &str) -> PyResult<String> {
    object.getattr(attribute)?.extract::<String>()
}

fn read_dict_string(dictionary: &Bound<'_, PyDict>, key: &str) -> PyResult<String> {
    dictionary
        .get_item(key)?
        .ok_or_else(|| PyTypeError::new_err(format!("missing key {key:?}")))?
        .extract::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::python_class_from_source_for_tests::class_from_source;

    /// A class carrying what `@streamlib.processor` attaches.
    const DECLARED_CLASS_SOURCE: &str = "\
__name__ = 'my_app.filters'


class BlurProcessor:
    __streamlib_processor_declared__ = True
    __streamlib_processor_description__ = 'blurs'
    __streamlib_processor_execution__ = {'mode': 'reactive'}
    __streamlib_processor_scheduling_priority__ = None
    __streamlib_processor_input_ports__ = []
    __streamlib_processor_output_ports__ = []
";

    /// A class that drifted between the two fields would be a processor
    /// registered under a name its own helper process cannot import.
    #[test]
    fn the_identity_and_the_entrypoint_are_the_same_derived_string() {
        Python::initialize();
        Python::attach(|python| {
            let declared_class = class_from_source(python, DECLARED_CLASS_SOURCE, "BlurProcessor");
            let declaration = PythonProcessorDeclaration::read_from_class(&declared_class).unwrap();

            assert_eq!(
                declaration.descriptor.processor_class_import_path.as_str(),
                "my_app.filters:BlurProcessor"
            );
            assert_eq!(
                Some(
                    declaration
                        .descriptor
                        .processor_class_import_path
                        .as_str()
                        .to_string()
                ),
                declaration.descriptor.entrypoint,
            );
        });
    }

    // ---- the window contract, declared in both languages ----

    /// The wheel's own `@processor` grammar, run in the test interpreter.
    ///
    /// Embedded rather than imported: `cargo test` has no installed wheel on
    /// `sys.path`, and the point is to read a marker the real decorator built
    /// rather than one this test hand-wrote.
    const PROCESSOR_DECLARATION_MODULE_SOURCE: &str =
        include_str!("../python/streamlib/_processor_declaration.py");

    /// A namespace with the real decorator module already run in it.
    fn declaration_module_namespace(python: Python<'_>) -> Bound<'_, PyDict> {
        let namespace = PyDict::new(python);
        python
            .run(
                &std::ffi::CString::new(PROCESSOR_DECLARATION_MODULE_SOURCE).unwrap(),
                Some(&namespace),
                None,
            )
            .expect("the decorator module runs");
        namespace
    }

    /// Run the real decorator module, run `class_body_source` against it, and
    /// read the resulting class through the bridge the engine uses at `rt.add`.
    ///
    /// A refusal raised at decoration and one raised at the bridge both land
    /// in the `Err` arm, because to an author they are one refusal.
    fn read_python_declaration(class_body_source: &str) -> PyResult<PythonProcessorDeclaration> {
        Python::initialize();
        Python::attach(|python| {
            let namespace = declaration_module_namespace(python);

            let source = format!("__name__ = 'my_app.audio'\n\n\n{class_body_source}");
            python.run(
                &std::ffi::CString::new(source).unwrap(),
                Some(&namespace),
                None,
            )?;

            let declared_class = namespace
                .get_item("AudioConsumer")?
                .expect("the class bound");
            PythonProcessorDeclaration::read_from_class(&declared_class)
        })
    }

    /// The input ports a Python class declares, as the engine reads them.
    fn python_declared_ports(class_body_source: &str) -> Vec<PortDescriptor> {
        read_python_declaration(class_body_source)
            .expect("the declaration reads")
            .descriptor
            .inputs
    }

    /// The message a refused Python declaration hands a user.
    ///
    /// `expect_err` is not available here: the success type is a production
    /// type, and deriving `Debug` on it to satisfy a test would be the test
    /// reshaping library code.
    fn python_declaration_refusal(class_body_source: &str) -> String {
        match read_python_declaration(class_body_source) {
            Ok(_) => panic!("the declaration was accepted; a refusal was expected"),
            Err(refusal) => refusal.to_string(),
        }
    }

    /// The contract a Rust author declares with the `#[processor]` grammar,
    /// spelled for the same port the Python class below declares.
    #[streamlib::sdk::processor(
        execution = reactive,
        input(
            "audio",
            delivery_profile = "ordered",
            audio_window(
                sample_rate = 16_000,
                channels = 1,
                dtype = "f32",
                window_size = 512,
                hop = 160
            )
        ),
    )]
    struct RustDeclaredAudioConsumer;

    impl streamlib::sdk::processors::ReactiveProcessor for RustDeclaredAudioConsumer::Processor {
        fn process(
            &mut self,
            _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
        ) -> streamlib::sdk::error::Result<()> {
            Ok(())
        }
    }

    /// The headline: one contract, two authoring languages, one schema.
    ///
    /// Both halves are read from the surfaces an author actually writes — the
    /// `@input` decorator and the `#[processor]` attribute — so a divergence
    /// in either grammar fails here rather than reaching a user.
    #[test]
    fn a_python_declared_contract_and_a_rust_declared_one_are_the_same_schema() {
        let python_ports = python_declared_ports(
            "@processor\n\
             class AudioConsumer:\n\
             \x20   @input('audio', delivery_profile='ordered',\n\
             \x20          audio_window=AudioWindowContract(sample_rate=16_000, channels=1,\n\
             \x20                                           dtype='f32', window_size=512, hop=160))\n\
             \x20   def audio_from_microphone(self): ...\n",
        );

        let rust_descriptor =
            <RustDeclaredAudioConsumer::Processor as streamlib::sdk::processors::GeneratedProcessor>::descriptor()
                .expect("the macro emits a descriptor");

        assert_eq!(python_ports.len(), 1);
        assert_eq!(
            python_ports[0].audio_window, rust_descriptor.inputs[0].audio_window,
            "the two authoring surfaces must produce one contract"
        );
        assert_eq!(
            serde_json::to_value(&python_ports[0].audio_window).unwrap(),
            serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 160,
            })
        );
    }

    /// A helper-placed processor opens no device stream, so the sentinel it
    /// would need to settle never resolves — and the decorator says so at the
    /// line the author wrote, not three seams later in placement vocabulary.
    #[test]
    fn a_python_declared_sentinel_is_refused_at_decoration() {
        let refusal = python_declaration_refusal(
            "@processor(execution='manual')\n\
             class AudioConsumer:\n\
             \x20   @input('audio', delivery_profile='ordered',\n\
             \x20          audio_window=AUDIO_WINDOW_MATCH_DEVICE)\n\
             \x20   def audio_from_device(self): ...\n",
        );

        assert!(
            refusal.contains("AUDIO_WINDOW_MATCH_DEVICE")
                && refusal.contains("helper")
                && refusal.contains("AudioWindowContract"),
            "the refusal must name the sentinel, why it cannot resolve, and what to \
             write instead; got {refusal}"
        );
    }

    #[test]
    fn a_python_port_declaring_no_contract_reaches_the_descriptor_with_none() {
        let ports = python_declared_ports(
            "@processor\n\
             class AudioConsumer:\n\
             \x20   @input('audio', delivery_profile='newest')\n\
             \x20   def audio_from_microphone(self): ...\n",
        );

        assert_eq!(ports[0].audio_window, None);
        assert_eq!(ports[0].delivery_profile.as_deref(), Some("newest"));
    }

    #[test]
    fn a_python_contract_beside_a_skipping_profile_is_refused_naming_both_knobs() {
        let refusal = python_declaration_refusal(
            "@processor\n\
             class AudioConsumer:\n\
             \x20   @input('audio', delivery_profile='newest',\n\
             \x20          audio_window=AudioWindowContract(sample_rate=16_000, channels=1,\n\
             \x20                                           dtype='f32', window_size=512))\n\
             \x20   def audio_from_microphone(self): ...\n",
        );

        assert!(
            refusal.contains("audio_window")
                && refusal.contains("newest")
                && refusal.contains("ordered"),
            "the refusal must name both knobs; got {refusal}"
        );
    }

    /// A class carrying a hand-built port marker — the case the decorator's
    /// own validation never sees.
    fn hand_built_marker_source(audio_window_fields: &str) -> String {
        format!(
            "__name__ = 'my_app.audio'


class AudioConsumer:
    __streamlib_processor_declared__ = True
    __streamlib_processor_description__ = ''
    __streamlib_processor_execution__ = {{'mode': 'reactive'}}
    __streamlib_processor_scheduling_priority__ = None
    __streamlib_processor_input_ports__ = [{{
        'name': 'audio',
        'description': '',
        'delivery_profile': 'ordered',
        'audio_window': {{{audio_window_fields}}},
    }}]
    __streamlib_processor_output_ports__ = []
"
        )
    }

    /// Read a hand-built marker through the bridge the engine uses at `rt.add`.
    fn read_hand_built_marker(audio_window_fields: &str) -> PyResult<PythonProcessorDeclaration> {
        Python::initialize();
        Python::attach(|python| {
            let source = hand_built_marker_source(audio_window_fields);
            let declared_class = class_from_source(python, &source, "AudioConsumer");
            PythonProcessorDeclaration::read_from_class(&declared_class)
        })
    }

    /// The message a hand-built marker's refusal hands a user.
    fn hand_built_marker_refusal(audio_window_fields: &str) -> String {
        match read_hand_built_marker(audio_window_fields) {
            Ok(_) => panic!("the marker was accepted; a refusal was expected"),
            Err(refusal) => refusal.to_string(),
        }
    }

    /// The decorator refuses the sentinel; this bridge does not, and must not.
    /// A marker the decorator never built still carries `match_device` through
    /// to the compiler, where the wire-time refusal — which knows the port's
    /// placement, as nothing here does — is the guard that speaks.
    #[test]
    fn a_hand_built_match_device_marker_still_reaches_the_bridge() {
        let declaration = read_hand_built_marker("'resolved_from': 'match_device'")
            .expect("the bridge reads a hand-built sentinel");

        assert_eq!(declaration.descriptor.inputs.len(), 1);
        assert_eq!(
            declaration.descriptor.inputs[0].audio_window,
            Some(AudioWindowContract::MatchDevice {})
        );
    }

    /// A marker built by something other than the decorator still meets the
    /// refusals: the wheel is never the only guard.
    #[test]
    fn a_hand_built_marker_smuggling_a_bad_contract_is_refused_at_the_bridge() {
        let refusal = hand_built_marker_refusal(
            "'resolved_from': 'declaration', 'sample_rate': 16000, 'channels': 1, \
             'dtype': 'f32', 'window_size': 512, 'hop': 4096",
        );

        assert!(
            refusal.contains("4096") && refusal.contains("512"),
            "the refusal must name both numbers; got {refusal}"
        );
    }

    #[test]
    fn a_hand_built_marker_with_a_negative_count_is_refused_naming_the_field() {
        let refusal = hand_built_marker_refusal(
            "'resolved_from': 'declaration', 'sample_rate': -1, 'channels': 1, \
             'dtype': 'f32', 'window_size': 512, 'hop': 512",
        );

        assert!(
            refusal.contains("sample_rate") && refusal.contains("-1"),
            "the refusal must name the field and the value; got {refusal}"
        );
    }

    /// The count is the one value a marker may leave out, and the bridge must
    /// carry the omission through rather than refuse it: a port that follows
    /// its source is spelled by saying nothing.
    #[test]
    fn a_hand_built_marker_omitting_its_channel_count_follows_the_source() {
        for spelling in [
            "'resolved_from': 'declaration', 'sample_rate': 48000, 'dtype': 'f32', \
             'window_size': 960, 'hop': 960",
            "'resolved_from': 'declaration', 'sample_rate': 48000, 'channels': 'source', \
             'dtype': 'f32', 'window_size': 960, 'hop': 960",
        ] {
            let declaration =
                read_hand_built_marker(spelling).expect("an omitted count is a whole contract");

            assert_eq!(
                declaration.descriptor.inputs[0].audio_window,
                Some(AudioWindowContract::Declaration(
                    AudioWindowContractDeclaredValues {
                        sample_rate: 48_000,
                        channels: None,
                        dtype: "f32".to_string(),
                        window_size: 960,
                        hop: 960,
                    }
                ))
            );
        }
    }

    #[test]
    fn a_hand_built_marker_whose_channels_names_no_count_is_refused_offering_the_spelling() {
        let refusal = hand_built_marker_refusal(
            "'resolved_from': 'declaration', 'sample_rate': 48000, 'channels': 'stereo', \
             'dtype': 'f32', 'window_size': 960, 'hop': 960",
        );

        assert!(
            refusal.contains("channels") && refusal.contains("source"),
            "the refusal must name the field and offer the spelling that works; got {refusal}"
        );
    }

    /// Every field the contract requires names the port and the contract when
    /// it is missing — none falls through to a bare `missing key`.
    ///
    /// `channels` is not among them: it is the one value a port may leave to
    /// its source, and its own test below is that omitting it is *accepted*.
    #[test]
    fn a_marker_missing_any_required_contract_field_is_refused_naming_the_port_and_the_field() {
        for missing_field in [
            "resolved_from",
            "sample_rate",
            "dtype",
            "window_size",
            "hop",
        ] {
            let fields = [
                ("resolved_from", "'declaration'"),
                ("sample_rate", "16000"),
                ("channels", "1"),
                ("dtype", "'f32'"),
                ("window_size", "512"),
                ("hop", "512"),
            ]
            .into_iter()
            .filter(|(name, _)| *name != missing_field)
            .map(|(name, value)| format!("'{name}': {value}"))
            .collect::<Vec<_>>()
            .join(", ");

            let refusal = hand_built_marker_refusal(&fields);
            assert!(
                refusal.contains("input port \"audio\"") && refusal.contains(missing_field),
                "a missing {missing_field:?} must name the port and the field; got {refusal}"
            );
        }
    }

    /// An output port declares no contract — the invariant three carrier docs
    /// state and the `#[processor]` grammar refuses. A hand-built marker is
    /// the only way to reach it, since `output()` takes no such argument.
    #[test]
    fn a_hand_built_output_marker_declaring_a_contract_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let source = "\
__name__ = 'my_app.audio'


class AudioConsumer:
    __streamlib_processor_declared__ = True
    __streamlib_processor_description__ = ''
    __streamlib_processor_execution__ = {'mode': 'manual'}
    __streamlib_processor_scheduling_priority__ = None
    __streamlib_processor_input_ports__ = []
    __streamlib_processor_output_ports__ = [{
        'name': 'windows',
        'description': '',
        'audio_window': {'resolved_from': 'match_device'},
    }]
";
            let declared_class = class_from_source(python, source, "AudioConsumer");

            let refusal = match PythonProcessorDeclaration::read_from_class(&declared_class) {
                Ok(_) => panic!("an output contract was accepted; a refusal was expected"),
                Err(refusal) => refusal.to_string(),
            };
            assert!(
                refusal.contains("output port \"windows\"") && refusal.contains("consuming"),
                "the refusal must name the port and whose setting it is; got {refusal}"
            );
        });
    }

    /// The dtype vocabulary is spelled once per language, and nothing but this
    /// keeps the two from drifting: a third dtype added on the Rust side would
    /// otherwise be refused by the Python decorator before the bridge that
    /// accepts it ever runs.
    #[test]
    fn both_languages_legalise_the_same_window_dtypes() {
        Python::initialize();
        Python::attach(|python| {
            let namespace = declaration_module_namespace(python);

            let python_dtypes = namespace
                .get_item("_AUDIO_WINDOW_DTYPES")
                .unwrap()
                .expect("the decorator module names its dtypes")
                .extract::<Vec<String>>()
                .expect("the dtypes are strings");

            assert_eq!(
                python_dtypes,
                streamlib::sdk::descriptors::AUDIO_WINDOW_DTYPE_DECLARATION_VALUES
                    .map(String::from)
                    .to_vec()
            );
        });
    }
}
