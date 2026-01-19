// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2-based IPC communication layer for cross-process processor communication.

mod input;
mod mailbox;
mod node;
mod output;
mod payload;

pub use input::InputMailboxes;
pub use mailbox::PortMailbox;
pub use node::{Iceoryx2Node, Iceoryx2Service};
pub use output::OutputWriter;
pub use payload::{FramePayload, PortKey, SchemaName, MAX_PAYLOAD_SIZE, MAX_SCHEMA_NAME_SIZE};
