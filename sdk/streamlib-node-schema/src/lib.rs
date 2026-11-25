// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Types shared between `streamlib` and `streamlib-macros` for code generation.

mod execution_config;
mod process_execution;
mod stated_lists;
mod thread_priority;

pub mod audio_window_contract;
pub mod config_schema_document;
pub mod description_lint;
pub mod descriptors;
pub mod error;
pub mod exposed_name_cast;
pub mod node_class_import_path;
pub mod node_class_short_name;
pub mod node_kind;
pub mod node_reference_in_config;
pub mod node_schema;
pub mod node_settings;
pub mod node_value_lifetime;
pub mod output_port_annotations;
pub mod pin_group;
#[cfg(test)]
mod pin_group_tests;
pub mod pin_type;
pub mod port_stream_facts;

pub use execution_config::ExecutionConfig;
pub use process_execution::ProcessExecution;
pub use thread_priority::ThreadPriority;

pub use audio_window_contract::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    AudioWindowContract, AudioWindowContractDeclaredValues,
    refuse_audio_window_beside_a_skipping_delivery_profile, render_declaration_values,
};
pub use config_schema_document::NodeConfigJsonSchema;
pub use error::{SchemaError, SchemaResult};
pub use exposed_name_cast::{
    EXPOSED_NAME_MAXIMUM_LENGTH, ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe,
    is_in_exposed_name_cast_form,
};
pub use node_class_import_path::{
    BUILT_IN_NODE_CLASS_MODULE, CONSTANT_NODE_TYPE, GET_VARIABLE_NODE_TYPE, GRAPH_INPUT_NODE_TYPE,
    GRAPH_OUTPUT_NODE_TYPE, IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR, NodeClassImportPath,
    SET_VARIABLE_NODE_TYPE, is_a_graph_interface_node_type, is_a_node_type_of_the_graph_itself,
};
pub use node_class_short_name::NodeClassShortName;
pub use node_kind::{FunctionNodeLanguage, NodeKind};
pub use node_reference_in_config::{
    ConfigKeyHoldingANodeReference, NODE_REFERENCE_NODE_KEY, NODE_REFERENCE_SCHEMA_KEYWORD,
    NodeReferenceInConfig, config_keys_holding_a_node_reference, inputs_holding_a_node_reference,
    node_name_a_config_value_references,
};
pub use node_schema::{
    DEFAULT_INPUT_PORT_BORROW_QUOTA, DELIVERY_PROFILE_DECLARATION_VALUES,
    MAX_INPUT_PORT_BORROW_QUOTA, NEWEST_DELIVERY_PROFILE, NodeConfigSchema, NodeLanguage,
    NodePortSchema, NodeScheduling, NodeSchema, NodeSchemaExecution, NodeStateField,
    ORDERED_DELIVERY_PROFILE, RuntimeConfig, RuntimeOptions, to_pascal_case, to_snake_case,
};
pub use node_settings::{
    NodeSettings, SettingChoosingAPinType, SettingChosenFromOptions, SettingPinType,
    the_choice_among_the_bundles_its_stream_declares, the_default_of_a_setting,
    the_input_of_a_setting, the_menu_of_a_setting,
};
pub use node_value_lifetime::NodeValueLifetime;
pub use output_port_annotations::{
    ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    ImageOutputPortFrameRate, output_port_frame_rate_resolved_against_config,
    the_pin_type_a_choice_holds,
};
pub use pin_group::{
    PIN_GROUP_MOST_SLOTS, PIN_GROUP_SLOT_PORT_NAME_SEPARATOR, PinGroupDescriptor,
    PinGroupSlotCount, PinGroupSlotPort,
};
pub use port_stream_facts::{
    ImageStreamContentLightLevel, ImageStreamFacts, ImageStreamFrameRate,
    ImageStreamMasteringDisplayColourVolume, PacketsStreamFacts, PortStreamFacts,
    StreamFactsOfAnotherPinType, refuse_stream_facts_of_another_pin_type,
};
