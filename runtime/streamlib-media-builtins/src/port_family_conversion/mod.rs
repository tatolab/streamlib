// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The built-in nodes a person or an agent places between two port families,
//! so every conversion, and what it costs, is a node in the graph.

pub mod array_to_channels;
pub mod channels_to_array;
pub mod channels_to_records;
mod port_family_messages_read_by_a_conversion;
pub mod records_to_channels;

pub use array_to_channels::{ArrayToChannels, ArrayToChannelsConfig};
pub use channels_to_array::ChannelsToArray;
pub use channels_to_records::{ChannelsToRecords, ChannelsToRecordsConfig};
pub use records_to_channels::{RecordsToChannels, RecordsToChannelsConfig};
