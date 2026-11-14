// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Processor and port descriptor types for introspection.
//!
//! These live in the engine-free `streamlib-processor-schema` crate so the
//! engine and the `#[processor]` macro share one definition. The engine
//! re-exports them here so every `crate::core::descriptors::*` path resolves
//! unchanged.

pub use streamlib_processor_schema::config_schema_document::{
    ProcessorConfigFilledFromItsDefault, ProcessorConfigJsonSchema, ProcessorConfigKeyDescribedBy,
    ProcessorConfigKeyDescription, schema_carrying_the_declared_description,
};
pub use streamlib_processor_schema::description_lint::{
    DescribedDeclaration, DescriptionLintFinding, DescriptionLintProblem,
    lint_the_descriptions_of_a_node_type,
};
pub use streamlib_processor_schema::descriptors::{
    CodeExamples, PortDescriptor, ProcessorDescriptor, ProcessorRuntime,
};
pub use streamlib_processor_schema::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    ArrayOutputPortResidency, AudioWindowContract, AudioWindowContractDeclaredValues,
    ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType, FunctionNodeLanguage,
    IMAGE_TEXTURE_FORMAT_WIRE_NAMES, ImageOutputPortFrameRate, ImageStreamContentLightLevel,
    ImageStreamFacts, ImageStreamFrameRate, ImageStreamMasteringDisplayColourVolume,
    NODE_REFERENCE_SCHEMA_KEYWORD, NodeKind, NodeReferenceInConfig, NodeReferenceToNodeType,
    NodeTypeANodeReferenceNames, NodeValueLifetime, OutputPortDeclaredNames, PacketsStreamFacts,
    PortFamily, PortFamilyDeclaration, PortFamilyDeclarationDirection, PortFamilyLinkEndpoint,
    PortStreamFacts, ProcessorClassImportPath, ProcessorClassShortName, ProcessorScheduling,
    output_port_codec_resolved_against_config, output_port_family_resolved_against_config,
    output_port_frame_rate_resolved_against_config, output_port_names_resolved_against_config,
    refusal_of_a_link_between_port_families, refusal_of_required_names,
    refuse_audio_window_beside_a_skipping_delivery_profile, refuse_stream_facts_of_another_family,
};
