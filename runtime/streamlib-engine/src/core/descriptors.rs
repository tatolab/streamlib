// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor and port descriptor types for introspection.
//!
//! These live in the engine-free `streamlib-processor-schema` crate so the
//! engine and the `#[processor]` macro share one definition. The engine
//! re-exports them here so every `crate::core::descriptors::*` path resolves
//! unchanged.

pub use streamlib_processor_schema::config_schema_document::ProcessorConfigJsonSchema;
pub use streamlib_processor_schema::descriptors::{
    CodeExamples, PortDescriptor, ProcessorDescriptor, ProcessorRuntime,
};
pub use streamlib_processor_schema::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    ArrayOutputPortResidency, AudioEntry, AudioWindowContract, AudioWindowContractDeclaredValues,
    BinaryEntry, CatalogDataTrackMode, CatalogEntry, CatalogEntryIsNotAnAudioEntry, CatalogSection,
    ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType, IMAGE_TEXTURE_FORMAT_WIRE_NAMES,
    JsonEntry, NODE_REFERENCE_SCHEMA_KEYWORD, NodeReferenceInConfig, NodeReferenceToNodeType,
    NodeTypeANodeReferenceNames, OutputPortDeclaredNames, PortFamily, PortFamilyDeclaration,
    PortFamilyDeclarationDirection, PortFamilyLinkEndpoint, ProcessorClassImportPath,
    ProcessorClassShortName, ProcessorScheduling, TextEntry, VideoEntry,
    output_port_codec_resolved_against_config, output_port_family_resolved_against_config,
    output_port_names_resolved_against_config, refusal_of_a_link_between_port_families,
    refusal_of_required_names, refuse_audio_window_beside_a_skipping_delivery_profile,
};

pub use crate::core::output_port_catalog_entries::the_hang_map_a_catalog_section_lists_its_tracks_under;
