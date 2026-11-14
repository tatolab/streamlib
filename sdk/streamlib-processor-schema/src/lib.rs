// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Types shared between `streamlib` and `streamlib-macros` for code generation.

mod execution_config;
mod process_execution;
mod thread_priority;

pub mod audio_window_contract;
pub mod config_schema_document;
pub mod description_lint;
pub mod descriptors;
pub mod error;
pub mod exposed_name_cast;
pub mod node_kind;
pub mod node_reference_in_config;
pub mod node_value_lifetime;
pub mod port_family;
pub mod port_stream_facts;
pub mod processor_class_import_path;
pub mod processor_class_short_name;
pub mod processor_schema;

pub use execution_config::ExecutionConfig;
pub use process_execution::ProcessExecution;
pub use thread_priority::ThreadPriority;

// Processor schema re-exports
pub use audio_window_contract::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    AudioWindowContract, AudioWindowContractDeclaredValues,
    refuse_audio_window_beside_a_skipping_delivery_profile, render_declaration_values,
};
pub use config_schema_document::{
    ProcessorConfigFilledFromItsDefault, ProcessorConfigJsonSchema, ProcessorConfigKeyDescribedBy,
    ProcessorConfigKeyDescription,
};
pub use error::{SchemaError, SchemaResult};
pub use exposed_name_cast::{
    EXPOSED_NAME_MAXIMUM_LENGTH, ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe,
    is_in_exposed_name_cast_form,
};
pub use node_kind::{FunctionNodeLanguage, NodeKind};
pub use node_reference_in_config::{
    ConfigKeyHoldingANodeReference, NODE_REFERENCE_NODE_KEY, NODE_REFERENCE_SCHEMA_KEYWORD,
    NODE_REFERENCE_SCHEMA_ONE_REFERRER_PER_REFERENCED_NODE_KEY, NodeReferenceInConfig,
    NodeReferenceToNodeType, NodeTypeANodeReferenceNames, config_keys_holding_a_node_reference,
    node_name_a_config_value_references,
};
pub use node_value_lifetime::NodeValueLifetime;
pub use port_family::{
    ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    IMAGE_TEXTURE_FORMAT_WIRE_NAMES, ImageOutputPortFrameRate, OutputPortDeclaredNames, PortFamily,
    PortFamilyDeclaration, PortFamilyDeclarationDirection, PortFamilyLinkEndpoint,
    output_port_codec_resolved_against_config, output_port_family_resolved_against_config,
    output_port_frame_rate_resolved_against_config, output_port_names_resolved_against_config,
    refusal_of_a_link_between_port_families, refusal_of_required_names,
};
pub use port_stream_facts::{
    ImageStreamContentLightLevel, ImageStreamFacts, ImageStreamFrameRate,
    ImageStreamMasteringDisplayColourVolume, PacketsStreamFacts, PortStreamFacts,
    StreamFactsOfAnotherFamily, refuse_stream_facts_of_another_family,
};
pub use processor_class_import_path::{
    BUILT_IN_NODE_CLASS_MODULE, IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR, ProcessorClassImportPath,
};
pub use processor_class_short_name::ProcessorClassShortName;
pub use processor_schema::{
    DEFAULT_INPUT_PORT_BORROW_QUOTA, DELIVERY_PROFILE_DECLARATION_VALUES,
    MAX_INPUT_PORT_BORROW_QUOTA, ProcessorConfigSchema, ProcessorLanguage, ProcessorPortSchema,
    ProcessorScheduling, ProcessorSchema, ProcessorSchemaExecution, ProcessorStateField,
    RuntimeConfig, RuntimeOptions, to_pascal_case, to_snake_case,
};
