// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wrapper types for passing typed link readers/writers with their link IDs.

use crate::core::graph::LinkUniqueId;
use crate::core::links::{LinkInputDataReader, LinkOutputDataWriter};
use crate::core::LinkPortMessage;

/// Trait for schema-tagged wrappers that can be validated across dylib boundaries.
///
/// This enables schema-name-based validation instead of TypeId-based downcast,
/// which is necessary for dynamic plugin loading where the same type compiled
/// in different compilation units has different TypeIds.
pub trait SchemaTagged: Send {
    /// Returns the schema name this wrapper was created for.
    fn schema_name(&self) -> &'static str;
}

/// Wrapper for passing LinkOutputDataWriter with its LinkUniqueId through Box<dyn Any>.
pub struct LinkOutputDataWriterWrapper<T: LinkPortMessage> {
    pub link_id: LinkUniqueId,
    pub schema_name: &'static str,
    pub data_writer: LinkOutputDataWriter<T>,
}

impl<T: LinkPortMessage> SchemaTagged for LinkOutputDataWriterWrapper<T> {
    fn schema_name(&self) -> &'static str {
        self.schema_name
    }
}

/// Wrapper for passing LinkInputDataReader with its LinkUniqueId through Box<dyn Any>.
pub struct LinkInputDataReaderWrapper<T: LinkPortMessage> {
    pub link_id: LinkUniqueId,
    pub schema_name: &'static str,
    pub data_reader: LinkInputDataReader<T>,
}

impl<T: LinkPortMessage> SchemaTagged for LinkInputDataReaderWrapper<T> {
    fn schema_name(&self) -> &'static str {
        self.schema_name
    }
}
