// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream listing's engine values as the wire names them.

use streamlib::sdk::error::PrivacyGatedCaptureDevice;
use streamlib::sdk::runtime::{StreamActionStep, StreamListingState, StreamLocation};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CaptureDeviceOnTheWire, ListedStreamState, StreamActionStepOnTheWire, StreamLocationOnTheWire,
};

pub(crate) fn stream_location_on_the_wire(
    stream_location: StreamLocation,
) -> StreamLocationOnTheWire {
    StreamLocationOnTheWire {
        project_directory: stream_location.project_directory,
        project: stream_location.project,
        stream_asset: stream_location.stream_asset,
    }
}

pub(crate) fn listed_stream_state_on_the_wire(state: StreamListingState) -> ListedStreamState {
    match state {
        StreamListingState::Running => ListedStreamState::Running,
        StreamListingState::Stopped => ListedStreamState::Stopped,
        StreamListingState::Paused => ListedStreamState::Paused,
        StreamListingState::Failed => ListedStreamState::Failed,
        StreamListingState::Preparing => ListedStreamState::Preparing,
        StreamListingState::Stopping => ListedStreamState::Stopping,
        StreamListingState::Closed => ListedStreamState::Closed,
    }
}

pub(super) fn capture_device_on_the_wire(
    device: PrivacyGatedCaptureDevice,
) -> CaptureDeviceOnTheWire {
    match device {
        PrivacyGatedCaptureDevice::Camera => CaptureDeviceOnTheWire::Camera,
        PrivacyGatedCaptureDevice::Microphone => CaptureDeviceOnTheWire::Microphone,
    }
}

pub(crate) fn stream_action_step_on_the_wire(step: StreamActionStep) -> StreamActionStepOnTheWire {
    match step {
        StreamActionStep::MakingTheVenv => StreamActionStepOnTheWire::MakingTheVenv,
        StreamActionStep::ReadingTheGraphDocuments => {
            StreamActionStepOnTheWire::ReadingTheGraphDocuments
        }
        StreamActionStep::Starting => StreamActionStepOnTheWire::Starting,
        StreamActionStep::Stopping => StreamActionStepOnTheWire::Stopping,
    }
}
