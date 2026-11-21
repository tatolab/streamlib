// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{SchemaError, SchemaResult};

/// The fully-qualified import path of the class a node is —
/// `my_app.filters:BlurNode` from Python, `my_app::filters::BlurNode`
/// from Rust.
///
/// Two spellings of one concept, discriminated by the descriptor's sibling
/// `runtime` field and by nothing in the string itself. Stored verbatim and
/// never parsed — splitting on `:` or `::` to recover a short name re-invents
/// the identity grammar this type replaced.
///
/// Every inhabitant is valid: the inner string is private, [`Self::new`] and
/// [`Self::of_built_in_node`], which cannot be blank, are the only
/// constructors, there is no `Default`, and [`Deserialize`] validates rather
/// than inheriting `String`'s.
///
/// Two things are read: a path in [`BUILT_IN_NODE_CLASS_MODULE`] names one of
/// the runtime's built-ins, resolved from its own registry and never imported;
/// and a Python path led by an import's folder,
/// `imports/<folder>/<module>:<qualname>`, names a class in that import's own
/// node code, imported from that folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(transparent)]
pub struct NodeClassImportPath(String);

/// The module a stream imports every built-in node's class from, so a
/// built-in's type is `tatolab.stream:<Class>`.
pub const BUILT_IN_NODE_CLASS_MODULE: &str = "tatolab.stream";

/// The import path a graph names `GraphInput` by, in the built-in node module.
pub const GRAPH_INPUT_NODE_TYPE: &str = "tatolab.stream:GraphInput";

/// The import path a graph names `GraphOutput` by, in the built-in node module.
pub const GRAPH_OUTPUT_NODE_TYPE: &str = "tatolab.stream:GraphOutput";

/// The import path a graph names `SetVariable` by, in the built-in node module.
pub const SET_VARIABLE_NODE_TYPE: &str = "tatolab.stream:SetVariable";

/// The import path a graph names `GetVariable` by, in the built-in node module.
pub const GET_VARIABLE_NODE_TYPE: &str = "tatolab.stream:GetVariable";

/// Whether `node_type` is `GraphInput` or `GraphOutput`.
pub fn is_a_graph_interface_node_type(node_type: &str) -> bool {
    matches!(node_type, GRAPH_INPUT_NODE_TYPE | GRAPH_OUTPUT_NODE_TYPE)
}

/// Whether `node_type` is one the graph resolves itself, which no node code runs: `GraphInput`,
/// `GraphOutput`, `SetVariable` or `GetVariable`.
pub fn is_a_node_type_of_the_graph_itself(node_type: &str) -> bool {
    is_a_graph_interface_node_type(node_type)
        || matches!(node_type, SET_VARIABLE_NODE_TYPE | GET_VARIABLE_NODE_TYPE)
}

/// What ends the import folder leading a node type that names a class in an
/// import's own node code; neither a Python nor a Rust import path holds it.
pub const IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR: char = '/';

impl NodeClassImportPath {
    /// Whether this is `GraphInput`.
    pub fn is_the_graph_input_type(&self) -> bool {
        self.0 == GRAPH_INPUT_NODE_TYPE
    }

    /// The import path of `GraphOutput`.
    pub fn graph_output() -> Self {
        Self(GRAPH_OUTPUT_NODE_TYPE.to_owned())
    }

    /// Build a path, refusing one that names no class.
    ///
    /// Emptiness is the whole rule: the path holds two grammars at once, so
    /// there is no single one to check it against.
    pub fn new(import_path: impl Into<String>) -> SchemaResult<Self> {
        let import_path = import_path.into();
        if import_path.trim().is_empty() {
            return Err(SchemaError::InvalidName {
                name: import_path,
                reason: "a node's identity is the import path of the class it is, so the \
                         path has to name one — this one is blank. It is derived mechanically \
                         and never authored: the `#[node]` macro captures it at the \
                         expansion site in Rust, and the wheel reads `__module__` and \
                         `__qualname__` off the class in Python"
                    .to_string(),
            });
        }
        Ok(Self(import_path))
    }

    /// The type of the built-in node whose public class is `class_name` —
    /// never blank, the module standing before it.
    pub fn of_built_in_node(class_name: &str) -> Self {
        Self(format!("{BUILT_IN_NODE_CLASS_MODULE}:{class_name}"))
    }

    /// Whether this path names a class in [`BUILT_IN_NODE_CLASS_MODULE`].
    pub fn names_a_built_in_node(&self) -> bool {
        self.0
            .strip_prefix(BUILT_IN_NODE_CLASS_MODULE)
            .is_some_and(|after_the_module| after_the_module.starts_with(':'))
    }

    /// The type of the Python class `class_import_path`, `<module>:<qualname>`,
    /// in the node code of the import whose folder is `import_folder`, relative
    /// to the project holding it.
    pub fn of_a_class_in_an_import_folder(
        import_folder: &str,
        class_import_path: &str,
    ) -> SchemaResult<Self> {
        Self::new(format!(
            "{import_folder}{IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR}{class_import_path}"
        ))
    }

    /// The import folder, relative to the project, whose node code defines
    /// this type, and the class's `<module>:<qualname>` inside it; `None` for a
    /// type the project's own code, its venv or the runtime defines.
    pub fn import_folder_and_class_import_path(&self) -> Option<(&str, &str)> {
        let (import_folder, class_import_path) =
            self.0.rsplit_once(IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR)?;
        let (module_name, qualname) = class_import_path.split_once(':')?;
        (!import_folder.is_empty()
            && !module_name.is_empty()
            && !qualname.is_empty()
            && !class_import_path.contains("::"))
        .then_some((import_folder, class_import_path))
    }

    /// The path as written by the authoring surface.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeClassImportPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NodeClassImportPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// A plain string in the control contract, which reads a node's `type` and never checks its
/// grammar: the runtime refuses a path it cannot read where the path is given.
#[cfg(feature = "contract-json-schema")]
impl schemars_1::JsonSchema for NodeClassImportPath {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NodeClassImportPath".into()
    }

    fn json_schema(_generator: &mut schemars_1::SchemaGenerator) -> schemars_1::Schema {
        schemars_1::json_schema!({ "type": "string" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYTHON_PATH: &str = "my_app.filters:BlurNode";
    const RUST_PATH: &str = "my_app::filters::BlurNode";

    #[test]
    fn both_grammars_are_admitted_verbatim() {
        for path in [PYTHON_PATH, RUST_PATH, "app:P", "crate::P"] {
            assert_eq!(
                NodeClassImportPath::new(path).unwrap().as_str(),
                path,
                "the path must survive construction byte for byte"
            );
        }
    }

    #[test]
    fn a_built_in_nodes_type_is_its_class_in_the_stream_package() {
        let audio_to_tensor = NodeClassImportPath::of_built_in_node("AudioToTensor");

        assert_eq!(audio_to_tensor.as_str(), "tatolab.stream:AudioToTensor");
        assert!(audio_to_tensor.names_a_built_in_node());
    }

    #[test]
    fn only_a_class_in_the_stream_package_itself_names_a_built_in_node() {
        for not_a_built_in in [
            PYTHON_PATH,
            RUST_PATH,
            "tatolab.streaming:CameraSource",
            "tatolab.stream.nodes:CameraSource",
            "streamlib_media_builtins::camera_source::CameraSource",
        ] {
            assert!(
                !NodeClassImportPath::new(not_a_built_in)
                    .unwrap()
                    .names_a_built_in_node(),
                "{not_a_built_in}"
            );
        }
    }

    #[test]
    fn a_class_in_an_import_folder_reads_back_its_folder_and_its_import_path() {
        let gripper = NodeClassImportPath::of_a_class_in_an_import_folder(
            "imports/robot-arm/imports/claws",
            "nodes.gripper:Gripper",
        )
        .unwrap();

        assert_eq!(
            gripper.as_str(),
            "imports/robot-arm/imports/claws/nodes.gripper:Gripper"
        );
        assert_eq!(
            gripper.import_folder_and_class_import_path(),
            Some(("imports/robot-arm/imports/claws", "nodes.gripper:Gripper"))
        );
    }

    #[test]
    fn a_path_with_no_import_folder_names_no_import_folder() {
        for not_in_an_import in [
            PYTHON_PATH,
            RUST_PATH,
            "tatolab.stream:CameraSource",
            "/nodes.gripper:Gripper",
            "imports/robot-arm/nodes.gripper",
            "imports/robot-arm/:Gripper",
        ] {
            assert_eq!(
                NodeClassImportPath::new(not_in_an_import)
                    .unwrap()
                    .import_folder_and_class_import_path(),
                None,
                "{not_in_an_import}"
            );
        }
    }

    #[test]
    fn a_blank_path_names_no_class_and_is_refused() {
        for blank in ["", " ", "\t", "\n", "   \t\n  "] {
            let refusal = NodeClassImportPath::new(blank)
                .expect_err("a path that names no class must be refused");
            assert!(
                refusal.to_string().contains("derived mechanically"),
                "the refusal must say where the path comes from, since the author never \
                 typed it: {refusal}"
            );
        }
    }

    /// Interior and surrounding whitespace is preserved, not trimmed: the
    /// engine stores the authoring surface's own string, and a silent trim
    /// would be the first normalization.
    #[test]
    fn a_path_with_whitespace_around_a_name_is_kept_verbatim() {
        assert_eq!(
            NodeClassImportPath::new(" app:P ").unwrap().as_str(),
            " app:P ",
            "construction must not normalize — only blankness is judged"
        );
    }

    #[test]
    fn it_serializes_as_a_plain_json_string() {
        let value = serde_json::to_value(NodeClassImportPath::new(PYTHON_PATH).unwrap())
            .expect("serialize");
        assert_eq!(
            value,
            serde_json::Value::String(PYTHON_PATH.to_string()),
            "the wire form is the bare string, not an object wrapping one"
        );
    }

    /// The hazard a plain newtype would have left open: `derive(Deserialize)`
    /// on a wrapper around `String` inherits `String`'s impl, which accepts
    /// `""` and reconstructs the invalid value the constructor refuses.
    #[test]
    fn deserialize_refuses_what_the_constructor_refuses() {
        for blank in [r#""""#, r#""   ""#] {
            let refused: Result<NodeClassImportPath, _> = serde_json::from_str(blank);
            assert!(
                refused.is_err(),
                "deserializing {blank} must not smuggle in a path the constructor rejects"
            );
        }
        assert!(
            rmp_serde::from_slice::<NodeClassImportPath>(&rmp_serde::to_vec_named(&"").unwrap())
                .is_err(),
            "the msgpack wire must refuse it too — validation belongs to the type, not to one \
             serializer"
        );
    }

    #[test]
    fn it_round_trips_over_json_and_msgpack() {
        for path in [PYTHON_PATH, RUST_PATH] {
            let original = NodeClassImportPath::new(path).unwrap();

            let back: NodeClassImportPath =
                serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
            assert_eq!(original, back, "lost equality over JSON");

            let back: NodeClassImportPath =
                rmp_serde::from_slice(&rmp_serde::to_vec_named(&original).unwrap()).unwrap();
            assert_eq!(original, back, "lost equality over msgpack");
        }
    }

    /// Registry behavior, at the type: distinct paths are distinct keys and the
    /// same path is the same key, with no case folding or separator
    /// equivalence in between.
    #[test]
    fn equality_is_exact_so_the_registry_key_is_the_path_itself() {
        let path = |p: &str| NodeClassImportPath::new(p).unwrap();

        assert_eq!(path(PYTHON_PATH), path(PYTHON_PATH));
        assert_ne!(path(PYTHON_PATH), path(RUST_PATH));
        assert_ne!(
            path("my_app.filters:BlurNode"),
            path("my_app.filters:blurnode"),
            "case is significant — Python's names are"
        );
        assert_ne!(
            path("my_app.filters:Blur"),
            path("my_app::filters::Blur"),
            "the two grammars are different strings, never canonicalized into one"
        );
    }

    #[test]
    fn display_renders_the_path_alone() {
        assert_eq!(
            NodeClassImportPath::new(PYTHON_PATH).unwrap().to_string(),
            PYTHON_PATH
        );
    }
}
