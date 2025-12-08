// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value as JsonValue;

use super::JsonComponent;
use crate::core::links::LinkOutputToProcessorMessage;

/// Writer and reader pair for messages from LinkOutput to this processor.
pub struct LinkOutputToProcessorWriterAndReader {
    pub writer: Sender<LinkOutputToProcessorMessage>,
    pub reader: Option<Receiver<LinkOutputToProcessorMessage>>,
}

impl LinkOutputToProcessorWriterAndReader {
    /// Create a new writer and reader pair.
    pub fn new() -> Self {
        let (writer, reader) = crossbeam_channel::unbounded();
        Self {
            writer,
            reader: Some(reader),
        }
    }

    /// Take the reader (can only be done once).
    pub fn take_reader(&mut self) -> Option<Receiver<LinkOutputToProcessorMessage>> {
        self.reader.take()
    }
}

impl Default for LinkOutputToProcessorWriterAndReader {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonComponent for LinkOutputToProcessorWriterAndReader {
    fn json_key(&self) -> &'static str {
        "link_output_to_processor_channel"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "attached": true,
            "reader_taken": self.reader.is_none(),
            "pending_messages": self.writer.len()
        })
    }
}
