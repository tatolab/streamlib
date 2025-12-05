// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Messages sent from LinkOutput to processors.

/// Message sent from a LinkOutput to a processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutputToProcessorMessage {
    /// Data is available, invoke the process function now.
    InvokeProcessingNow,
    /// Stop processing data and shut down.
    StopProcessingNow,
}
