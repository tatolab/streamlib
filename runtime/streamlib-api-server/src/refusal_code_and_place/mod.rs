// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The code and the place of a refusal that comes from a typed source: one exhaustive match over
//! the project repository's refusals and one over the engine's errors, so a new case does not
//! compile until it has a code.

use streamlib::sdk::descriptors::PinLinkRefusal;
use streamlib::sdk::error::{Error, PortDirection, WhyANodeOfAStreamIsNotRunning};
use streamlib_project_repository::ProjectRepositoryRefusal;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, NodeNotRunningBecauseOnTheWire, ProjectActionRefusalOnTheWire,
    ProjectFilePlaceOnTheWire,
};

/// What a refusal tells a caller beside its words: its code, and the node, input, file or
/// reason it names.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct RefusalCodeAndPlace {
    pub(crate) refusal_code: ControlOperationRefusalCode,
    pub(crate) refused_node: Option<String>,
    pub(crate) refused_field: Option<String>,
    pub(crate) refused_file: Option<ProjectFilePlaceOnTheWire>,
    pub(crate) not_running_because: Option<NodeNotRunningBecauseOnTheWire>,
}

impl RefusalCodeAndPlace {
    /// A refusal coded `refusal_code` that names no place.
    pub(crate) fn coded(refusal_code: ControlOperationRefusalCode) -> Self {
        Self {
            refusal_code,
            ..Self::default()
        }
    }

    /// A refusal coded `refusal_code` about the project file at `path`.
    fn of_the_file(refusal_code: ControlOperationRefusalCode, path: &str) -> Self {
        Self {
            refusal_code,
            refused_file: Some(ProjectFilePlaceOnTheWire {
                path: path.to_owned(),
                line: None,
                column: None,
            }),
            ..Self::default()
        }
    }

    /// The refusal of `action` this code and place make, in `runtime_words`.
    pub(crate) fn on_the_refusal_of(
        self,
        action: &str,
        runtime_words: impl Into<String>,
    ) -> ProjectActionRefusalOnTheWire {
        ProjectActionRefusalOnTheWire {
            refused_node: self.refused_node,
            refused_field: self.refused_field,
            refused_file: self.refused_file,
            not_running_because: self.not_running_because,
            ..ProjectActionRefusalOnTheWire::of_the_action(action, self.refusal_code, runtime_words)
        }
    }

    /// The code and place of a project repository's refusal.
    pub(crate) fn of_a_project_repository_refusal(refusal: &ProjectRepositoryRefusal) -> Self {
        use ControlOperationRefusalCode::{
            FileChangedSinceItWasRead, FileNotFound, Refused, RuntimeFailed, StreamNotFound,
        };
        match refusal {
            ProjectRepositoryRefusal::NoSuchProjectFile(path)
            | ProjectRepositoryRefusal::NoSuchProjectFolder(path) => {
                Self::of_the_file(FileNotFound, path)
            }
            ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRead { path, .. }
            | ProjectRepositoryRefusal::ProjectFileRemovedSinceItWasRead(path) => {
                Self::of_the_file(FileChangedSinceItWasRead, path)
            }
            ProjectRepositoryRefusal::ProjectFilePathRefused { path, .. }
            | ProjectRepositoryRefusal::ProjectFileAlreadyExists(path)
            | ProjectRepositoryRefusal::ProjectFileWrittenByAnotherAction { path, .. }
            | ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRecorded { path }
            | ProjectRepositoryRefusal::ProjectFileAlreadyHoldsThisContent(path)
            | ProjectRepositoryRefusal::AssetPathRefused { path, .. }
            | ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset { path, .. }
            | ProjectRepositoryRefusal::AssetPathHoldsAnotherFile { path }
            | ProjectRepositoryRefusal::GraphOperationRefused { path, .. }
            | ProjectRepositoryRefusal::GraphDocumentUnreadable { path, .. }
            | ProjectRepositoryRefusal::StreamDocumentEditedSinceItWasWritten { path } => {
                Self::of_the_file(Refused, path)
            }
            ProjectRepositoryRefusal::NoSuchStream(_) => Self::coded(StreamNotFound),
            ProjectRepositoryRefusal::ConfigReferencesNoAsset { node, .. } => Self {
                refused_node: Some(node.to_string()),
                ..Self::coded(Refused)
            },
            ProjectRepositoryRefusal::ProjectFileUnreadable { path, .. }
            | ProjectRepositoryRefusal::AssetSidecarUnreadable { path, .. }
            | ProjectRepositoryRefusal::PyprojectUnreadable { path, .. } => {
                Self::of_the_file(Refused, &path.to_string_lossy())
            }
            ProjectRepositoryRefusal::Io(_)
            | ProjectRepositoryRefusal::History(_)
            | ProjectRepositoryRefusal::Json(_) => Self::coded(RuntimeFailed),
            ProjectRepositoryRefusal::NotAProject { .. }
            | ProjectRepositoryRefusal::AlreadyAProject { .. }
            | ProjectRepositoryRefusal::ProjectDirectoryAlreadyExists { .. }
            | ProjectRepositoryRefusal::ProjectArchiveIsAFolder { .. }
            | ProjectRepositoryRefusal::ProjectAlreadyCommitted { .. }
            | ProjectRepositoryRefusal::ProjectDirectoryNotAbsolute(_)
            | ProjectRepositoryRefusal::AssetNamedTwice { .. }
            | ProjectRepositoryRefusal::NoSuchAsset(_)
            | ProjectRepositoryRefusal::StreamAssetsAreNotImported
            | ProjectRepositoryRefusal::UploadImportedWithNoPathNamesNoFileName
            | ProjectRepositoryRefusal::ImportFileNameRefused { .. }
            | ProjectRepositoryRefusal::ExpectedSha256IsNotADigest(_)
            | ProjectRepositoryRefusal::StreamAssetsAreNotReplaced(_)
            | ProjectRepositoryRefusal::StreamAssetHasNoImportSettings(_)
            | ProjectRepositoryRefusal::AssetStillReferenced { .. }
            | ProjectRepositoryRefusal::BlankProjectName
            | ProjectRepositoryRefusal::CoverIsNotAMediaAsset { .. }
            | ProjectRepositoryRefusal::AssetIsTheProjectCover(_)
            | ProjectRepositoryRefusal::DuplicateIntoAnExistingPath { .. }
            | ProjectRepositoryRefusal::DuplicateIntoTheProjectItself { .. }
            | ProjectRepositoryRefusal::DisplayNameShowsNothing(_)
            | ProjectRepositoryRefusal::NothingToUndo { .. }
            | ProjectRepositoryRefusal::NothingToRedo { .. }
            | ProjectRepositoryRefusal::ImportsDeclareOneDistribution { .. }
            | ProjectRepositoryRefusal::ImportRequirementsUnresolvable { .. }
            | ProjectRepositoryRefusal::ImportSourceUnreadable { .. }
            | ProjectRepositoryRefusal::NoSuchUpload(_)
            | ProjectRepositoryRefusal::UploadHoldsOtherBytes { .. }
            | ProjectRepositoryRefusal::EmptyTransaction
            | ProjectRepositoryRefusal::AuthorWithoutAName
            | ProjectRepositoryRefusal::TrackedFileAndFolderShareAName { .. }
            | ProjectRepositoryRefusal::ProjectImportsItself(_)
            | ProjectRepositoryRefusal::ImportAlreadyHeld(_)
            | ProjectRepositoryRefusal::NoSuchImport(_)
            | ProjectRepositoryRefusal::ImportFolderAlreadyHeld { .. }
            | ProjectRepositoryRefusal::CoreProjectImportFolderTaken { .. }
            | ProjectRepositoryRefusal::ImportFolderRefused { .. }
            | ProjectRepositoryRefusal::ImportSourceNotAnAbsolutePath { .. }
            | ProjectRepositoryRefusal::ImportSourceHoldsNoProject { .. }
            | ProjectRepositoryRefusal::ImportSourceHoldsAnotherProject { .. }
            | ProjectRepositoryRefusal::ImportHoldsNoAsset { .. }
            | ProjectRepositoryRefusal::AssetFileMissing { .. }
            | ProjectRepositoryRefusal::ImportEditedSinceItsCopy { .. }
            | ProjectRepositoryRefusal::ImportStillReferenced { .. }
            | ProjectRepositoryRefusal::BlankIdempotencyKey
            | ProjectRepositoryRefusal::ImportSourceNeitherAPathNorAnUpload(_)
            | ProjectRepositoryRefusal::TransactionFormatNewerThanThisRuntime { .. }
            | ProjectRepositoryRefusal::NoSuchSampleProject { .. }
            | ProjectRepositoryRefusal::SampleProjectMainStreamHoldsNoAsset { .. }
            | ProjectRepositoryRefusal::SampleProjectCopyWouldOverwrite { .. } => {
                Self::coded(Refused)
            }
        }
    }

    /// The code and place of an engine's error. A variant that holds only words answers
    /// `refused`, `Runtime` and `ShaderCompilation` among them, since the engine words
    /// refusals a caller can act on with them. The words-only variants that answer
    /// `runtime_failed` are `GpuError`, `TextureError`, `BufferError`, `ClockError`, `Io`,
    /// `EngineResourceRefusedAtAStreamsStart`, `ReservedPublisherSubscriberSlotOccupied`,
    /// `BagDecodeFailed` and `BagEncodeFailed`, each a failure of the runtime's own resources.
    /// A variant with fields is coded by what it names.
    pub(crate) fn of_an_engine_error(refusal: &Error) -> Self {
        use ControlOperationRefusalCode::{
            InputTakesNoWire, LinkAlreadyExists, LinkPinTypesDoNotConnect, NodeNameTaken,
            NodeNotFound, NodeNotRunning, NodeTypeNotFound, PortNotFound, Refused, RuntimeFailed,
        };
        match refusal {
            Error::NodeOfAStreamIsNotRunning {
                not_running_because,
                ..
            } => Self {
                not_running_because: Some(match not_running_because {
                    WhyANodeOfAStreamIsNotRunning::PlaybackStopped => {
                        NodeNotRunningBecauseOnTheWire::PlaybackStopped
                    }
                    WhyANodeOfAStreamIsNotRunning::PlaybackPaused => {
                        NodeNotRunningBecauseOnTheWire::PlaybackPaused
                    }
                    WhyANodeOfAStreamIsNotRunning::Preparing => {
                        NodeNotRunningBecauseOnTheWire::Preparing
                    }
                    WhyANodeOfAStreamIsNotRunning::NodeFailed => {
                        NodeNotRunningBecauseOnTheWire::NodeFailed
                    }
                }),
                ..Self::coded(NodeNotRunning)
            },
            Error::NodePortNotFound {
                port_name,
                direction,
                ..
            }
            | Error::NodeTypePortNotFound {
                port_name,
                direction,
                ..
            } => Self {
                refused_field: (*direction == PortDirection::Input).then(|| port_name.clone()),
                ..Self::coded(PortNotFound)
            },
            Error::StreamHasNoInputNamed { input, .. } => Self {
                refused_field: Some(input.clone()),
                ..Self::coded(PortNotFound)
            },
            Error::NodeInAStreamHasNoOutputPortNamed { node, .. } => Self {
                refused_node: Some(node.clone()),
                ..Self::coded(PortNotFound)
            },
            Error::NodeNotFound(_) => Self::coded(NodeNotFound),
            Error::UnknownNodeType { .. } | Error::BuiltInNodeTypeMovedToAModule { .. } => {
                Self::coded(NodeTypeNotFound)
            }
            Error::NodeNameTaken { .. } => Self::coded(NodeNameTaken),
            Error::LinkAlreadyExists(_) => Self::coded(LinkAlreadyExists),
            Error::PinLinkRefused(refusal) => match refusal.as_ref() {
                PinLinkRefusal::InputTakesNoWire {
                    node_name,
                    input_name,
                } => Self {
                    refused_node: Some(node_name.clone()),
                    refused_field: Some(input_name.clone()),
                    ..Self::coded(InputTakesNoWire)
                },
                PinLinkRefusal::PinTypesDoNotConnect { .. } => {
                    Self::coded(LinkPinTypesDoNotConnect)
                }
            },
            Error::GpuError(_)
            | Error::EngineResourceRefusedAtAStreamsStart(_)
            | Error::TextureError(_)
            | Error::BufferError(_)
            | Error::SurfaceFrameRecycled { .. }
            | Error::EverySlotInTheNodeOutputPoolIsInUse { .. }
            | Error::GpuPoolClaimantColumnsExhausted { .. }
            | Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld { .. }
            | Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget { .. }
            | Error::FrameAllocationRefusedNearTheDeviceMemoryAllocationCountLimit { .. }
            | Error::FrameAllocationRefusedPastTheMetalWorkingSetBudget { .. }
            | Error::ClockError(_)
            | Error::Io(_)
            | Error::ReservedPublisherSubscriberSlotOccupied(_)
            | Error::BagDecodeFailed(_)
            | Error::BagEncodeFailed(_)
            | Error::ChannelSampleStampsAPayloadPastTheSample { .. } => Self::coded(RuntimeFailed),
            Error::Runtime(_)
            | Error::ShaderCompilation(_)
            | Error::GraphError(_)
            | Error::PortError(_)
            | Error::Link(_)
            | Error::LinkNotFound(_)
            | Error::LinkNotWired(_)
            | Error::LinkAlreadyDisconnected(_)
            | Error::InvalidLink(_)
            | Error::ChannelNameChunkIsNotOneAddressChunk { .. }
            | Error::ChannelNameTooLong { .. }
            | Error::ExposedNameCastsToNothing(_)
            | Error::OutputPortNotExposedToTheReader(_)
            | Error::InvalidGraph(_)
            | Error::NodeTypesNotDescribed { .. }
            | Error::NodeConfigRefused { .. }
            | Error::DescriptorPortNameNotCast { .. }
            | Error::DescriptorPortNameDeclaredTwice { .. }
            | Error::StreamFactsOfAnotherFamily(_)
            | Error::Configuration(_)
            | Error::CaptureDeviceAccessNeverAskedFor { .. }
            | Error::Config(_)
            | Error::NotSupported(_)
            | Error::NotFound(_)
            | Error::BagKeyMissing { .. }
            | Error::BagTypeMismatch { .. }
            | Error::PayloadExceedsChannelCeiling { .. }
            | Error::AudioWindowStageCannotReadTheBag { .. }
            | Error::AudioWindowStageChannelConversionRefused { .. }
            | Error::Other(_) => Self::coded(Refused),
        }
    }
}

#[cfg(test)]
mod tests;
