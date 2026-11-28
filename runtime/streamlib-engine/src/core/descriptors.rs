// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

pub use streamlib_node_schema::config_schema_document::{
    NodeConfigJsonSchema, schema_carrying_the_declared_description,
};
pub use streamlib_node_schema::description_lint::{
    DescribedDeclaration, DescriptionLintFinding, DescriptionLintProblem,
    lint_the_descriptions_of_a_node_type,
};
pub use streamlib_node_schema::descriptors::{
    CodeExamples, NodeDescriptor, NodeRuntime, PortDescriptor,
};
pub use streamlib_node_schema::pin_type::{
    BuiltInPinTypeName, BundleMember, ConvertNodeToInsert, DeclaredPinType, FloatSize,
    FreePinConversion, IntegerSize, NamedPinTypeImportPath, NodeInputMode, PinAnnotationName,
    PinAnnotations, PinConversion, PinConversionOutputPick, PinConversionOutputPicked,
    PinConversionSavedValueCast, PinDeclaration, PinFlow, PinLinkEnd, PinLinkEndNamed,
    PinLinkInputEnd, PinLinkOutcome, PinLinkRefusal, PinLinkVerdict, PinLinkWarning,
    PinNumberRange, PinProjectAssetAnnotation, PinType, PinTypesAChoiceTakes, PinValueNotTaken,
    ProjectAssetMedia, ResolvedPin, SavedValueNotCast, SavedValueRetyped, TexturePixelFormat,
    VectorComponentCount, WhyASavedValueTakesTheDefault, an_input_takes_a_wire, built_in_pin_type,
    pin_link_verdict, the_integer_a_float_is_cast_to, the_number_cast, the_saved_value_retyped,
    the_value_held_within_the_range,
};
pub use streamlib_node_schema::value_input_declaration::{
    ValueInputChoosingAPinType, ValueInputChosenFromOptions, ValueInputPinType,
    the_choice_among_the_bundles_its_stream_declares, the_input_taking_no_wire,
    the_menu_of_a_value_input,
};
pub use streamlib_node_schema::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    ArrayOutputPortResidency, AudioWindowContract, AudioWindowContractDeclaredValues,
    ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType, FunctionNodeLanguage,
    ImageOutputPortFrameRate, ImageStreamContentLightLevel, ImageStreamFacts, ImageStreamFrameRate,
    ImageStreamMasteringDisplayColourVolume, NODE_REFERENCE_SCHEMA_KEYWORD, NodeClassImportPath,
    NodeClassShortName, NodeKind, NodeReferenceInConfig, NodeScheduling, NodeValueLifetime,
    PIN_GROUP_MOST_SLOTS, PacketsStreamFacts, PinGroupDescriptor, PinGroupSlotCount,
    PinGroupSlotPort, PortStreamFacts, output_port_frame_rate_resolved_against_config,
    refuse_audio_window_beside_a_skipping_delivery_profile,
    refuse_stream_facts_of_another_pin_type,
};
