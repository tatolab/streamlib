// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared traits for link ports.

pub mod link_buffer_read_mode;
pub mod link_port_address;
pub mod link_port_message;
pub mod link_port_type;

pub use link_buffer_read_mode::LinkBufferReadMode;
pub use link_port_address::LinkPortAddress;
pub use link_port_message::{sealed, LinkPortMessage};
pub use link_port_type::LinkPortType;
