// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort;

/// The `match_device` contracts this processor's input ports have settled,
/// shared live with the mailboxes that settled them.
///
/// Stored but never rendered as a key of its own — it implements
/// [`StorableComponent`] and not [`Component`], so there is no rendering to
/// leave unused. It exists so a port declaring `audio_window = match_device`
/// renders the five values its device gave rather than the sentinel that asked
/// for them, which is machine-dependent because the device format is — and
/// truer than a static lie.
///
/// [`StorableComponent`]: crate::core::graph::StorableComponent
/// [`Component`]: crate::core::graph::Component
pub struct DeviceMatchedAudioWindowContractsComponent(
    pub Arc<DeviceMatchedAudioWindowContractsByInputPort>,
);
