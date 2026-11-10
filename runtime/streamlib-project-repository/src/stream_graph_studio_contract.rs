// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `studio` contract Tatolab owns and versions: the fields a node is drawn with, and the
//! canvas notes a graph document holds beside its nodes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use uuid::Uuid;

/// The newest `studio` contract this runtime writes and reads; it reads every older one.
pub const STUDIO_CONTRACT_VERSION: u64 = 1;

/// The `studio` key naming the contract version a `studio` object is written under.
const STUDIO_CONTRACT_KEY: &str = "contract";

/// The `studio` key holding what a client keeps beside the fixed keys, kept as given.
const STUDIO_METADATA_KEY: &str = "metadata";

/// A point or a size on the canvas, `[x, y]` or `[width, height]`.
pub type StudioCanvasPair = [Number; 2];

/// What a node is drawn with, under the `studio` contract: each fixed key checked, anything
/// else under `metadata`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Map<String, Value>", into = "Map<String, Value>")]
pub struct StreamGraphNodeStudioFields {
    /// Where the node sits on the canvas, `[x, y]`.
    pub position: Option<StudioCanvasPair>,
    /// The labelled group the node belongs to, by its UUID.
    pub group: Option<Uuid>,
    /// A note the canvas shows on the node.
    pub comment: Option<String>,
    /// The colour the canvas draws the node with.
    pub colour: Option<String>,
    /// Whether the canvas draws the node collapsed.
    pub collapsed: Option<bool>,
    /// What a client keeps beside the fixed keys, kept as given.
    pub metadata: Map<String, Value>,
}

impl StreamGraphNodeStudioFields {
    /// Whether no field is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Read the `studio` a caller sends, always under the newest contract: a fixed key of the
    /// wrong shape and any other key outside `metadata` is refused by name.
    pub fn from_a_callers_studio(mut studio: Map<String, Value>) -> Result<Self, String> {
        studio
            .entry(STUDIO_CONTRACT_KEY)
            .or_insert_with(|| Value::from(STUDIO_CONTRACT_VERSION));
        Self::try_from(studio)
    }

    /// The fields as the `studio` object a graph document holds.
    pub fn to_map(&self) -> Map<String, Value> {
        self.clone().into()
    }
}

/// How one fixed key of the node `studio` contract reads its value.
enum StudioFieldShape {
    CanvasPair,
    Uuid,
    Text,
    Flag,
}

impl StudioFieldShape {
    fn of_the_key(key: &str) -> Option<Self> {
        match key {
            "position" => Some(Self::CanvasPair),
            "group" => Some(Self::Uuid),
            "comment" | "colour" => Some(Self::Text),
            "collapsed" => Some(Self::Flag),
            _ => None,
        }
    }

    fn what_it_takes(&self) -> &'static str {
        match self {
            Self::CanvasPair => "an `[x, y]` pair of numbers",
            Self::Uuid => "a UUID",
            Self::Text => "a string",
            Self::Flag => "`true` or `false`",
        }
    }

    fn reads(&self, value: &Value) -> bool {
        match self {
            Self::CanvasPair => serde_json::from_value::<StudioCanvasPair>(value.clone()).is_ok(),
            Self::Uuid => value
                .as_str()
                .is_some_and(|text| Uuid::parse_str(text).is_ok()),
            Self::Text => value.is_string(),
            Self::Flag => value.is_boolean(),
        }
    }
}

/// The contract version `studio` is written under, `None` for a free map written before the
/// contract; refused when it names no version this runtime reads.
fn the_contract_version_of(studio: &Map<String, Value>) -> Result<Option<u64>, String> {
    match studio.get(STUDIO_CONTRACT_KEY) {
        None => Ok(None),
        Some(version) => match version.as_u64() {
            Some(version @ 1..=STUDIO_CONTRACT_VERSION) => Ok(Some(version)),
            _ => Err(format!(
                "`studio.{STUDIO_CONTRACT_KEY}` is {version}, and this runtime reads the \
                 contract versions 1 to {STUDIO_CONTRACT_VERSION}"
            )),
        },
    }
}

impl TryFrom<Map<String, Value>> for StreamGraphNodeStudioFields {
    type Error = String;

    /// Read a node's `studio`: under a contract, each fixed key of the wrong shape and each
    /// other key is refused by name; a free map written before the contract keeps each key that
    /// does not read under `metadata`.
    fn try_from(mut studio: Map<String, Value>) -> Result<Self, Self::Error> {
        let under_a_contract = the_contract_version_of(&studio)?.is_some();
        studio.remove(STUDIO_CONTRACT_KEY);
        let mut metadata = match studio.remove(STUDIO_METADATA_KEY) {
            None => Map::new(),
            Some(Value::Object(metadata)) => metadata,
            Some(other) if !under_a_contract => {
                let mut metadata = Map::new();
                metadata.insert(STUDIO_METADATA_KEY.to_owned(), other);
                metadata
            }
            Some(_) => return Err("`studio.metadata` is a JSON object".to_owned()),
        };
        let mut fields = Self::default();
        for (key, value) in studio {
            let Some(shape) = StudioFieldShape::of_the_key(&key) else {
                if under_a_contract {
                    return Err(format!(
                        "`studio.{key}` is not a key of the studio contract, which takes \
                         `position`, `group`, `comment`, `colour`, `collapsed` and `metadata`; \
                         put anything else under `metadata`"
                    ));
                }
                metadata.insert(key, value);
                continue;
            };
            if !shape.reads(&value) {
                if under_a_contract {
                    return Err(format!(
                        "`studio.{key}` is {value}, and it takes {}",
                        shape.what_it_takes()
                    ));
                }
                metadata.insert(key, value);
                continue;
            }
            match key.as_str() {
                "position" => fields.position = serde_json::from_value(value).ok(),
                "group" => {
                    fields.group = value.as_str().and_then(|text| Uuid::parse_str(text).ok())
                }
                "comment" => fields.comment = value.as_str().map(str::to_owned),
                "colour" => fields.colour = value.as_str().map(str::to_owned),
                _ => fields.collapsed = value.as_bool(),
            }
        }
        fields.metadata = metadata;
        Ok(fields)
    }
}

impl From<StreamGraphNodeStudioFields> for Map<String, Value> {
    fn from(fields: StreamGraphNodeStudioFields) -> Self {
        let mut studio = Map::new();
        if fields.is_empty() {
            return studio;
        }
        studio.insert(
            STUDIO_CONTRACT_KEY.to_owned(),
            Value::from(STUDIO_CONTRACT_VERSION),
        );
        let StreamGraphNodeStudioFields {
            position,
            group,
            comment,
            colour,
            collapsed,
            metadata,
        } = fields;
        if let Some([x, y]) = position {
            studio.insert(
                "position".to_owned(),
                Value::Array(vec![x.into(), y.into()]),
            );
        }
        if let Some(group) = group {
            studio.insert("group".to_owned(), Value::String(group.to_string()));
        }
        if let Some(comment) = comment {
            studio.insert("comment".to_owned(), Value::String(comment));
        }
        if let Some(colour) = colour {
            studio.insert("colour".to_owned(), Value::String(colour));
        }
        if let Some(collapsed) = collapsed {
            studio.insert("collapsed".to_owned(), Value::Bool(collapsed));
        }
        if !metadata.is_empty() {
            studio.insert(STUDIO_METADATA_KEY.to_owned(), Value::Object(metadata));
        }
        studio
    }
}

/// What a canvas note says and where it sits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamGraphNoteFields {
    /// The note's text.
    pub text: String,
    /// Where its top-left corner sits on the canvas, `[x, y]`.
    pub position: StudioCanvasPair,
    /// Its width and height on the canvas, `[width, height]`.
    pub size: StudioCanvasPair,
}

/// One canvas note of a graph document, under its `studio.notes`; it changes nothing that runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamGraphDocumentNote {
    /// The note's UUID, the runtime's from its placement on.
    pub id: Uuid,
    #[serde(flatten)]
    pub fields: StreamGraphNoteFields,
}

/// The `studio` key of a graph document holding its canvas notes.
const STUDIO_NOTES_KEY: &str = "notes";

/// The canvas notes a graph document's own `studio` object holds; none when it holds none.
pub(crate) fn the_notes_of_a_documents_studio(
    studio: Option<&Value>,
) -> Result<Vec<StreamGraphDocumentNote>, String> {
    let Some(studio) = studio else {
        return Ok(Vec::new());
    };
    let studio = studio
        .as_object()
        .ok_or("the graph's `studio` is a JSON object")?;
    the_contract_version_of(studio)?;
    if let Some(key) = studio
        .keys()
        .find(|key| ![STUDIO_CONTRACT_KEY, STUDIO_NOTES_KEY].contains(&key.as_str()))
    {
        return Err(format!(
            "the graph's `studio.{key}` is not a key of the studio contract, which takes \
             `notes` on a graph"
        ));
    }
    match studio.get(STUDIO_NOTES_KEY) {
        None => Ok(Vec::new()),
        Some(notes) => serde_json::from_value(notes.clone())
            .map_err(|not_read| format!("the graph's `studio.notes` do not read: {not_read}")),
    }
}

/// A graph document's own `studio` object holding `notes`; `None` when there is none.
pub(crate) fn a_documents_studio_holding(
    notes: &[StreamGraphDocumentNote],
) -> Result<Option<Value>, String> {
    if notes.is_empty() {
        return Ok(None);
    }
    let notes = serde_json::to_value(notes)
        .map_err(|not_written| format!("the notes do not write: {not_written}"))?;
    let mut studio = Map::new();
    studio.insert(
        STUDIO_CONTRACT_KEY.to_owned(),
        Value::from(STUDIO_CONTRACT_VERSION),
    );
    studio.insert(STUDIO_NOTES_KEY.to_owned(), notes);
    Ok(Some(Value::Object(studio)))
}
