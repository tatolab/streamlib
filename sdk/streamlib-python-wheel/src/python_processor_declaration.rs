// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading an `audio_window` channel count off a Python value, through the
//! engine's one parser of the stamp shape.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use streamlib::sdk::processor_interpreter::AudioWindowFieldRefusal;

use crate::python_bag_conversion::python_object_to_json_value;

/// Read a `channels` value written either as a count or as the
/// source-following spelling, raising a refusal behind `naming` — a
/// `TypeError` for a value of the wrong kind, a `ValueError` for one the field
/// cannot hold.
pub(crate) fn read_a_channel_count_or_the_source_spelling(
    value: &Bound<'_, PyAny>,
    naming: impl std::fmt::Display,
) -> PyResult<Option<u32>> {
    // A value JSON cannot carry is no count and no spelling, which the parser
    // refuses as the wrong kind of value, as it does `null`.
    let value = python_object_to_json_value(value).unwrap_or(serde_json::Value::Null);
    streamlib::sdk::processor_interpreter::read_a_channel_count_or_the_source_spelling(&value)
        .map_err(|refusal| match refusal {
            AudioWindowFieldRefusal::WrongKindOfValue(reason) => {
                PyTypeError::new_err(format!("{naming} {reason}"))
            }
            AudioWindowFieldRefusal::UnusableValue(reason) => {
                PyValueError::new_err(format!("{naming} {reason}"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::python_class_from_source_for_tests::class_from_source;
    use pyo3::types::{PyDict, PyList};
    use streamlib::sdk::descriptors::{
        AudioWindowContract, AudioWindowContractDeclaredValues, PortDescriptor,
        ProcessorClassImportPath, ProcessorConfigJsonSchema,
    };
    use streamlib::sdk::processor_interpreter::PythonProcessorDeclaration;
    use streamlib::sdk::processors::EmptyConfig;

    /// The described node type a processor interpreter prints for
    /// `declared_class`: its import path, short name, and every stamp the
    /// decorator left, carried verbatim.
    fn described_node_type_of(declared_class: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
        let stamp = |name: &str| -> PyResult<serde_json::Value> {
            python_object_to_json_value(&declared_class.getattr(name)?)
        };
        let module = declared_class.getattr("__module__")?.extract::<String>()?;
        let qualname = declared_class
            .getattr("__qualname__")?
            .extract::<String>()?;
        Ok(serde_json::json!({
            "import_path": format!("{module}:{qualname}"),
            "short_name": declared_class.getattr("__name__")?.extract::<String>()?,
            "description": stamp("__tatolab_node_description__")?,
            "execution": stamp("__tatolab_node_execution__")?,
            "scheduling_priority": stamp("__tatolab_node_scheduling_priority__")?,
            "config_schema": stamp("__tatolab_node_config_schema__")?,
            "input_ports": stamp("__tatolab_node_input_ports__")?,
            "output_ports": stamp("__tatolab_node_output_ports__")?,
        }))
    }

    /// Read `declared_class` the way the engine reads what a processor
    /// interpreter described of it.
    fn read_declaration_off(
        declared_class: &Bound<'_, PyAny>,
    ) -> Result<PythonProcessorDeclaration, String> {
        let described_node_type =
            described_node_type_of(declared_class).map_err(|refusal| refusal.to_string())?;
        let requested_import_path = ProcessorClassImportPath::new(
            described_node_type["import_path"]
                .as_str()
                .unwrap_or_default(),
        )
        .map_err(|blank| blank.to_string())?;
        PythonProcessorDeclaration::read_from_described_node_type(
            &described_node_type,
            &requested_import_path,
        )
    }

    // ---- the window contract, declared in both languages ----

    /// The stream distribution's source root, put on `sys.path` because a
    /// `cargo test` run has no installed `tatolab-stream`.
    const STREAM_DISTRIBUTION_SOURCE_ROOT_DIRECTORY: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../tatolab-stream");

    /// A fresh copy of the real `tatolab.stream._node_declaration` module's
    /// namespace, so a test reads a marker the real decorator built rather than
    /// one it hand-wrote.
    fn declaration_module_namespace(python: Python<'_>) -> Bound<'_, PyDict> {
        let sys_path = python
            .import("sys")
            .unwrap()
            .getattr("path")
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        if !sys_path
            .contains(STREAM_DISTRIBUTION_SOURCE_ROOT_DIRECTORY)
            .unwrap()
        {
            sys_path
                .insert(0, STREAM_DISTRIBUTION_SOURCE_ROOT_DIRECTORY)
                .unwrap();
        }
        python
            .import("tatolab.stream._node_declaration")
            .expect("the decorator module imports")
            .dict()
            .copy()
            .unwrap()
    }

    /// Run the real decorator module, run `class_body_source` against it, and
    /// read the resulting class through the bridge the engine reads a declared
    /// class by.
    ///
    /// A refusal raised at decoration and one raised at the bridge both land
    /// in the `Err` arm, because to an author they are one refusal.
    fn read_python_declaration(
        class_body_source: &str,
    ) -> Result<PythonProcessorDeclaration, String> {
        Python::initialize();
        Python::attach(|python| {
            let namespace = declaration_module_namespace(python);

            let source = format!("__name__ = 'my_app.audio'\n\n\n{class_body_source}");
            python
                .run(
                    &std::ffi::CString::new(source).unwrap(),
                    Some(&namespace),
                    None,
                )
                .map_err(|refusal| refusal.to_string())?;

            let declared_class = namespace
                .get_item("AudioConsumer")
                .map_err(|refusal| refusal.to_string())?
                .expect("the class bound");
            read_declaration_off(&declared_class)
        })
    }

    /// The input ports a Python class declares, as the engine reads them.
    fn python_declared_ports(class_body_source: &str) -> Vec<PortDescriptor> {
        read_python_declaration(class_body_source)
            .expect("the declaration reads")
            .descriptor
            .inputs
    }

    /// The catalog's whole point: an agent reads a processor's keys off the
    /// descriptor before the class is ever in a graph, so the document the
    /// decorator derived has to survive the trip into Rust intact.
    #[test]
    fn the_descriptor_carries_the_config_classs_derived_schema() {
        let declaration = read_python_declaration(
            "\
import dataclasses


@dataclasses.dataclass
class AudioConsumerConfig:
    gain: float
    label: str = 'unlabelled'


@node(execution='manual')
class AudioConsumer:
    def __init__(self, config: AudioConsumerConfig) -> None:
        self.config = config
",
        )
        .expect("the declaration reads");

        let document = declaration
            .descriptor
            .config_schema
            .expect("a declared Python class always carries a config schema");
        assert_eq!(document["type"], "object");
        assert_eq!(document["properties"]["gain"]["type"], "number");
        assert_eq!(document["properties"]["label"]["type"], "string");
        assert_eq!(document["properties"]["label"]["default"], "unlabelled");
        assert_eq!(document["required"], serde_json::json!(["gain"]));
        assert_eq!(document["additionalProperties"], false);
    }

    /// What `/api/registry` actually serializes for a Python class.
    ///
    /// The endpoint's own test registers a descriptor by hand, and the test
    /// above stops at the descriptor, so without this nothing in a GPU-free CI
    /// run carries a Python class's schema as far as the served shape — and the
    /// end-to-end proof needs a running graph, which needs a GPU.
    #[test]
    fn the_served_rendering_of_a_python_class_carries_its_config_schema() {
        let declaration = read_python_declaration(
            "\
import dataclasses
import typing


@dataclasses.dataclass
class AudioConsumerConfig:
    gain: float
    label: str = 'unlabelled'
    fallback: typing.Optional[str] = None


@node(execution='manual')
class AudioConsumer:
    def __init__(self, config: AudioConsumerConfig) -> None:
        self.config = config
",
        )
        .expect("the declaration reads");

        let served = serde_json::to_value(
            streamlib::sdk::json_schema::ProcessorDescriptorOutput::from(&declaration.descriptor),
        )
        .expect("the rendering serializes");

        assert_eq!(
            served["config_schema"]["properties"]["gain"]["type"],
            "number"
        );
        assert_eq!(
            served["config_schema"]["properties"]["label"]["default"],
            "unlabelled"
        );
        assert_eq!(
            served["config_schema"]["required"],
            serde_json::json!(["gain"])
        );

        // The null leg of the hop the document takes into Rust: a dropped key
        // would read as "no default" rather than as the default the author
        // wrote, and nothing else in a GPU-free run crosses a nil.
        let fallback = &served["config_schema"]["properties"]["fallback"];
        assert!(
            fallback.get("default").is_some(),
            "the null default was dropped: {fallback}"
        );
        assert_eq!(fallback["default"], serde_json::Value::Null);
    }

    /// A processor declaring no config publishes what `EmptyConfig` publishes
    /// in Rust, so one catalog reads one way whichever language declared the
    /// processor.
    ///
    /// `schemars` stamps a root `title` from the config type's name and no
    /// Python document carries one, so the comparison drops it.
    #[test]
    fn a_class_declaring_no_config_carries_the_same_document_rust_publishes() {
        let declaration = read_python_declaration(
            "\
@node(execution='manual')
class AudioConsumer:
    def __init__(self) -> None:
        self.frames = 0
",
        )
        .expect("the declaration reads");

        let mut what_rust_publishes = EmptyConfig::processor_config_schema_document();
        what_rust_publishes
            .as_object_mut()
            .expect("the document is an object")
            .remove("title");

        assert_eq!(
            declaration.descriptor.config_schema,
            Some(what_rust_publishes)
        );
    }

    /// The message a refused Python declaration hands a user.
    ///
    /// `expect_err` is not available here: the success type is a production
    /// type, and deriving `Debug` on it to satisfy a test would be the test
    /// reshaping library code.
    fn python_declaration_refusal(class_body_source: &str) -> String {
        match read_python_declaration(class_body_source) {
            Ok(_) => panic!("the declaration was accepted; a refusal was expected"),
            Err(refusal) => refusal,
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
    /// `@node.input` decorator and the `#[processor]` attribute — so a divergence
    /// in either grammar fails here rather than reaching a user.
    #[test]
    fn a_python_declared_contract_and_a_rust_declared_one_are_the_same_schema() {
        let python_ports = python_declared_ports(
            "@node\n\
             class AudioConsumer:\n\
             \x20   @node.input('audio', delivery_profile='ordered',\n\
             \x20               audio_window=AudioWindowContract(sample_rate=16_000, channels=1,\n\
             \x20                                                dtype='f32', window_size=512, hop=160))\n\
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
            "@node(execution='manual')\n\
             class AudioConsumer:\n\
             \x20   @node.input('audio', delivery_profile='ordered',\n\
             \x20               audio_window=AUDIO_WINDOW_MATCH_DEVICE)\n\
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
            "@node\n\
             class AudioConsumer:\n\
             \x20   @node.input('audio', delivery_profile='newest')\n\
             \x20   def audio_from_microphone(self): ...\n",
        );

        assert_eq!(ports[0].audio_window, None);
        assert_eq!(ports[0].delivery_profile.as_deref(), Some("newest"));
    }

    /// The catalog lists what the descriptor carries, so a port an author spelled
    /// `Video` has to arrive as the `video` every lookup casts to.
    #[test]
    fn a_python_port_reaches_the_descriptor_under_its_cast_name() {
        let declaration = read_python_declaration(
            "@node\n\
             class AudioConsumer:\n\
             \x20   @node.input(delivery_profile='newest')\n\
             \x20   def Video(self): ...\n\
             \x20   @node.output(name='Café Out')\n\
             \x20   def frames_to_downstream(self): ...\n",
        )
        .expect("the declaration reads");

        assert_eq!(declaration.descriptor.inputs[0].name, "video");
        assert_eq!(declaration.descriptor.outputs[0].name, "cafe-out");
    }

    #[test]
    fn a_python_contract_beside_a_skipping_profile_is_refused_naming_both_knobs() {
        let refusal = python_declaration_refusal(
            "@node\n\
             class AudioConsumer:\n\
             \x20   @node.input('audio', delivery_profile='newest',\n\
             \x20               audio_window=AudioWindowContract(sample_rate=16_000, channels=1,\n\
             \x20                                                dtype='f32', window_size=512))\n\
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
    __tatolab_node_declared__ = True
    __tatolab_node_description__ = ''
    __tatolab_node_execution__ = {{'mode': 'reactive'}}
    __tatolab_node_scheduling_priority__ = None
    __tatolab_node_config_schema__ = {{'type': 'object'}}
    __tatolab_node_input_ports__ = [{{
        'name': 'audio',
        'description': '',
        'delivery_profile': 'ordered',
        'audio_window': {{{audio_window_fields}}},
    }}]
    __tatolab_node_output_ports__ = []
"
        )
    }

    /// Read a hand-built marker through the bridge the engine reads a declared
    /// class by.
    fn read_hand_built_marker(
        audio_window_fields: &str,
    ) -> Result<PythonProcessorDeclaration, String> {
        Python::initialize();
        Python::attach(|python| {
            let source = hand_built_marker_source(audio_window_fields);
            let declared_class = class_from_source(python, &source, "AudioConsumer");
            read_declaration_off(&declared_class)
        })
    }

    /// The message a hand-built marker's refusal hands a user.
    fn hand_built_marker_refusal(audio_window_fields: &str) -> String {
        match read_hand_built_marker(audio_window_fields) {
            Ok(_) => panic!("the marker was accepted; a refusal was expected"),
            Err(refusal) => refusal,
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

    /// `bool` is an `int` subclass in Python, so a marker carrying `True` would
    /// otherwise reach the stage as one channel — a plausible count nobody
    /// wrote. The declaration constructor refuses one by name and so does this.
    #[test]
    fn a_hand_built_marker_carrying_a_bool_where_a_number_belongs_is_refused() {
        for (field, spelling) in [
            ("channels", "'channels': True"),
            ("sample_rate", "'sample_rate': True"),
            ("window_size", "'window_size': True"),
            ("hop", "'hop': True"),
        ] {
            let mut fields = vec![
                "'resolved_from': 'declaration'",
                "'sample_rate': 48000",
                "'channels': 2",
                "'dtype': 'f32'",
                "'window_size': 960",
                "'hop': 960",
            ];
            fields.retain(|written| !written.starts_with(&format!("'{field}'")));
            fields.push(spelling);

            let refusal = hand_built_marker_refusal(&fields.join(", "));
            assert!(
                refusal.contains(field) && refusal.contains("bool"),
                "a bool in {field:?} must be refused naming the field and the kind; \
                 got {refusal}"
            );
        }
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
    /// the only way to reach it, since `node.output()` takes no such argument.
    #[test]
    fn a_hand_built_output_marker_declaring_a_contract_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let source = "\
__name__ = 'my_app.audio'


class AudioConsumer:
    __tatolab_node_declared__ = True
    __tatolab_node_description__ = ''
    __tatolab_node_execution__ = {'mode': 'manual'}
    __tatolab_node_scheduling_priority__ = None
    __tatolab_node_config_schema__ = {'type': 'object'}
    __tatolab_node_input_ports__ = []
    __tatolab_node_output_ports__ = [{
        'name': 'windows',
        'description': '',
        'audio_window': {'resolved_from': 'match_device'},
    }]
";
            let declared_class = class_from_source(python, source, "AudioConsumer");

            let refusal = match read_declaration_off(&declared_class) {
                Ok(_) => panic!("an output contract was accepted; a refusal was expected"),
                Err(refusal) => refusal,
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
