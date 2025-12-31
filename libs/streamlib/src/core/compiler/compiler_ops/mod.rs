// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod prepare_processor_op;
mod spawn_processor_op;
mod wire_link_op;

pub use crate::core::links::{LinkInputDataReaderWrapper, LinkOutputDataWriterWrapper};
pub(crate) use prepare_processor_op::prepare_processor;
pub(crate) use spawn_processor_op::spawn_processor;
pub use wire_link_op::{unwire_link, wire_link};
