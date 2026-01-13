// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess RHI - Cross-process frame transport abstraction.
//!
//! Internal SPI for language binding developers (streamlib-python, streamlib-typescript).

mod broker;
mod channel;
mod frame_transport;

pub use broker::{BrokerInstallStatus, SubprocessRhiBroker};
pub use channel::{ChannelRole, SubprocessRhiChannel};
pub use frame_transport::{FrameTransportHandle, SubprocessRhiFrameTransport};
