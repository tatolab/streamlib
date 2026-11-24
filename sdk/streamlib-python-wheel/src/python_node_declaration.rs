// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Reading an `audio_window` channel count off a Python value, through the
//! engine's one parser of the stamp shape.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use streamlib::sdk::node_interpreter::AudioWindowFieldRefusal;

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
    streamlib::sdk::node_interpreter::read_a_channel_count_or_the_source_spelling(&value).map_err(
        |refusal| match refusal {
            AudioWindowFieldRefusal::WrongKindOfValue(reason) => {
                PyTypeError::new_err(format!("{naming} {reason}"))
            }
            AudioWindowFieldRefusal::UnusableValue(reason) => {
                PyValueError::new_err(format!("{naming} {reason}"))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::{PyDict, PyList};
    use streamlib::sdk::descriptors::{NodeClassImportPath, NodeConfigJsonSchema, PortDescriptor};
    use streamlib::sdk::node_interpreter::PythonNodeDeclaration;
    use streamlib::sdk::nodes::EmptyConfig;

    fn described_node_type_of(declared_class: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
        let stamp = |name: &str| -> PyResult<serde_json::Value> {
            python_object_to_json_value(&declared_class.getattr(name)?)
        };
        let module = declared_class.getattr("__module__")?.extract::<String>()?;
        let qualname = declared_class
            .getattr("__qualname__")?
            .extract::<String>()?;
        let mut described = serde_json::json!({
            "import_path": format!("{module}:{qualname}"),
            "short_name": declared_class.getattr("__name__")?.extract::<String>()?,
            "description": stamp("__tatolab_node_description__")?,
            "kind": stamp("__tatolab_node_kind__")?,
            "execution": stamp("__tatolab_node_execution__")?,
            "scheduling_priority": stamp("__tatolab_node_scheduling_priority__")?,
            "paced_by_real_time": stamp("__tatolab_node_paced_by_real_time__")?,
            "input_ports": stamp("__tatolab_node_input_ports__")?,
            "output_ports": stamp("__tatolab_node_output_ports__")?,
            "conversion": stamp("__tatolab_node_conversion__")?,
            "pin_groups": stamp("__tatolab_node_pin_groups__")?,
        });
        // Only a Rust function node stamps a config schema, as the node interpreter describes it.
        if declared_class.hasattr("__tatolab_node_config_schema__")? {
            described["config_schema"] = stamp("__tatolab_node_config_schema__")?;
        }
        Ok(described)
    }

    fn read_declaration_off(
        declared_class: &Bound<'_, PyAny>,
    ) -> Result<PythonNodeDeclaration, String> {
        let described_node_type =
            described_node_type_of(declared_class).map_err(|refusal| refusal.to_string())?;
        let requested_import_path = NodeClassImportPath::new(
            described_node_type["import_path"]
                .as_str()
                .unwrap_or_default(),
        )
        .map_err(|blank| blank.to_string())?;
        PythonNodeDeclaration::read_from_described_node_type(
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
        let namespace = python
            .import("tatolab.stream._node_declaration")
            .expect("the decorator module imports")
            .dict()
            .copy()
            .unwrap();
        namespace
            .update(
                python
                    .import("tatolab.stream")
                    .expect("the package imports")
                    .dict()
                    .as_mapping(),
            )
            .unwrap();
        namespace
    }

    /// Run the real decorator module, run `class_body_source` against it, and
    /// read the resulting class through the bridge the engine reads a declared
    /// class by.
    ///
    /// A refusal raised at decoration and one raised at the bridge both land
    /// in the `Err` arm, because to an author they are one refusal.
    fn read_python_declaration(class_body_source: &str) -> Result<PythonNodeDeclaration, String> {
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

    /// A class under bare `@node` reaches the catalog `describe_node_types` serves: each field
    /// an input of its type with its default, a `SettingOnly()` one a setting read at setup, one
    /// stating `InputValueLifetime("reset")` forgotten at Stop, and each pin of its type and flow.
    #[test]
    fn a_class_nodes_fields_and_pins_reach_the_catalog_with_their_modes_and_lifetimes() {
        let declaration = read_python_declaration(
            "\
from typing import Annotated, Literal


@node
class AudioConsumer:
    voice: InputPin[Annotated[Audio, StreamDelivery('ordered')]]
    quality: Annotated[Literal['fast', 'fine'], SettingOnly()] = 'fast'
    gain: Annotated[float, InputValueLifetime('reset')] = 1.0
    level: OutputPin[float]
",
        )
        .expect("the declaration reads");

        let described =
            streamlib::sdk::nodes::NodeTypePortsDescribedOnTheWire::of(&declaration.descriptor);
        let input = |name: &str| {
            serde_json::to_value(
                described
                    .inputs
                    .iter()
                    .find(|input| input.port.name == name)
                    .expect("the catalog lists the input"),
            )
            .unwrap()
        };
        assert_eq!(
            (
                &input("quality")["mode"],
                &input("quality")["read"],
                &input("quality")["default_value"],
                &input("quality")["pin_type"],
            ),
            (
                &serde_json::json!("setting_only"),
                &serde_json::json!("at_setup"),
                &serde_json::json!("fast"),
                &serde_json::json!({"kind": "is", "pin_type": {"type": "menu", "option_names": ["fast", "fine"]}}),
            )
        );
        assert_eq!(
            (
                &input("gain")["mode"],
                &input("gain")["lifetime"],
                &input("gain")["default_value"]
            ),
            (
                &serde_json::json!("wire_or_value"),
                &serde_json::json!("reset"),
                &serde_json::json!(1.0)
            )
        );
        assert_eq!(
            (&input("voice")["mode"], &input("voice")["flow"]),
            (
                &serde_json::json!("wire_only"),
                &serde_json::json!("stream")
            )
        );
        assert_eq!(
            declaration.descriptor.inputs[0].delivery_profile.as_deref(),
            Some("ordered")
        );
        assert_eq!(
            serde_json::to_value(&described.outputs[0]).unwrap()["flow"],
            serde_json::json!("value")
        );
    }

    /// A class under bare `@node` declaring `PinGroup[Slot]` reaches the catalog as a pin group,
    /// each field of its slot a pin of one slot, and its slot input that takes a wire making it
    /// follow its wires.
    #[test]
    fn a_class_nodes_pin_group_reaches_the_catalog_with_the_pins_of_one_slot() {
        let declaration = read_python_declaration(
            "\
from dataclasses import dataclass


@dataclass(frozen=True)
class Layer:
    picture: InputPin[Texture]
    opacity: float = 1.0


@node
class AudioConsumer:
    layers: PinGroup[Layer]
    result: OutputPin[Texture]
",
        )
        .expect("the declaration reads");

        let group = &declaration.descriptor.pin_groups[0];
        assert_eq!(
            (
                group.name.as_str(),
                group
                    .inputs
                    .iter()
                    .map(|port| port.name.as_str())
                    .collect::<Vec<_>>(),
                group.outputs.len(),
            ),
            ("layers", vec!["picture", "opacity"], 0)
        );
        let described =
            streamlib::sdk::nodes::NodeTypePortsDescribedOnTheWire::of(&declaration.descriptor);
        assert_eq!(
            serde_json::to_value(&described.pin_groups[0]).unwrap()["slots"],
            serde_json::json!("follow_their_wires")
        );
        assert!(
            declaration
                .descriptor
                .inputs
                .iter()
                .all(|port| !port.name.starts_with("layers"))
        );
    }

    /// A class under bare `@node` stating `conversion=True` reaches the catalog as a conversion
    /// from its one input that takes a wire to its one output, its setting left out of it.
    #[test]
    fn a_class_stating_a_conversion_reaches_the_catalog_as_one() {
        let declaration = read_python_declaration(
            "\
from typing import Annotated


@node.meta(conversion=True)
@node(execution='manual')
class AudioConsumer:
    bitrate_bps: Annotated[int, SettingOnly()] = 4_000_000
    video: InputPin[Texture]
    encoded_video: OutputPin[EncodedVideo]
",
        )
        .expect("the declaration reads");

        let conversion = declaration
            .descriptor
            .pin_conversion()
            .expect("its shape is a conversion's")
            .expect("it states a conversion");
        assert_eq!(
            (
                conversion.input_port_name.as_str(),
                conversion.output_port_name.as_str()
            ),
            ("video", "encoded_video")
        );
    }

    /// A class under bare `@node` takes each value as an input, so it describes no config, and
    /// the catalog serves it the document Rust publishes for a node that takes none.
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

        assert_eq!(
            declaration.descriptor.config_schema,
            Some(EmptyConfig::node_config_schema_document())
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

    #[streamlib::sdk::node(
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
            ),
            pin_type = Audio,
        ),
    )]
    struct RustDeclaredAudioConsumer;

    impl streamlib::sdk::nodes::ReactiveNode for RustDeclaredAudioConsumer::Node {
        fn process(
            &mut self,
            _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
        ) -> streamlib::sdk::error::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_python_declared_contract_and_a_rust_declared_one_are_the_same_schema() {
        let python_ports = python_declared_ports(
            "from typing import Annotated\n\
             @node\n\
             class AudioConsumer:\n\
             \x20   audio: InputPin[Annotated[Audio, StreamDelivery('ordered'),\n\
             \x20       AudioWindowContract(sample_rate=16_000, channels=1,\n\
             \x20                           dtype='f32', window_size=512, hop=160)]]\n",
        );

        let rust_descriptor =
            <RustDeclaredAudioConsumer::Node as streamlib::sdk::nodes::GeneratedNode>::descriptor()
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

    #[test]
    fn a_python_port_declaring_no_contract_reaches_the_descriptor_with_none() {
        let ports = python_declared_ports(
            "from typing import Annotated\n\
             @node\n\
             class AudioConsumer:\n\
             \x20   audio: InputPin[Annotated[Audio, StreamDelivery('newest')]]\n",
        );

        assert_eq!(ports[0].audio_window, None);
        assert_eq!(ports[0].delivery_profile.as_deref(), Some("newest"));
    }

    /// The catalog lists what the descriptor carries, so a port an author spelled
    /// `Video` has to arrive as the `video` every lookup casts to.
    #[test]
    fn a_python_port_reaches_the_descriptor_under_its_cast_name() {
        let declaration = read_python_declaration(
            "from typing import Annotated\n\
             @node\n\
             class AudioConsumer:\n\
             \x20   Video: InputPin[Annotated[Texture, StreamDelivery('newest')]]\n\
             \x20   Frames_Out: OutputPin[Texture]\n",
        )
        .expect("the declaration reads");

        assert_eq!(declaration.descriptor.inputs[0].name, "video");
        assert_eq!(declaration.descriptor.outputs[0].name, "frames_out");
    }

    #[test]
    fn a_python_contract_beside_a_skipping_profile_is_refused_naming_both_knobs() {
        let refusal = python_declaration_refusal(
            "from typing import Annotated\n\
             @node\n\
             class AudioConsumer:\n\
             \x20   audio: InputPin[Annotated[Audio, StreamDelivery('newest'),\n\
             \x20       AudioWindowContract(sample_rate=16_000, channels=1,\n\
             \x20                           dtype='f32', window_size=512)]]\n",
        );

        assert!(
            refusal.contains("audio_window")
                && refusal.contains("newest")
                && refusal.contains("ordered"),
            "the refusal must name both knobs; got {refusal}"
        );
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
