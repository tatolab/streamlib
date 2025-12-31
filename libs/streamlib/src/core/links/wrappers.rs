// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wrapper types for passing typed link readers/writers with their link IDs.

use crate::core::graph::LinkUniqueId;
use crate::core::links::{LinkInputDataReader, LinkOutputDataWriter};
use crate::core::LinkPortMessage;

/// Wrapper for passing LinkOutputDataWriter with its LinkUniqueId through Box<dyn Any>.
pub struct LinkOutputDataWriterWrapper<T: LinkPortMessage> {
    pub link_id: LinkUniqueId,
    pub data_writer: LinkOutputDataWriter<T>,
}

/// Wrapper for passing LinkInputDataReader with its LinkUniqueId through Box<dyn Any>.
pub struct LinkInputDataReaderWrapper<T: LinkPortMessage> {
    pub link_id: LinkUniqueId,
    pub data_reader: LinkInputDataReader<T>,
}
