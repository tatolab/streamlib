// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort;

pub struct DeviceMatchedAudioWindowContractsComponent(
    pub Arc<DeviceMatchedAudioWindowContractsByInputPort>,
);
