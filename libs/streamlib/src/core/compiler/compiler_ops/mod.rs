// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod create_processor_op;
mod setup_processor_op;
mod shutdown_processor_op;
mod start_processor_op;
mod wire_link_op;

pub(crate) use create_processor_op::create_processor;
pub(crate) use setup_processor_op::setup_processor;
pub use shutdown_processor_op::{shutdown_all_processors, shutdown_processor};
pub(crate) use start_processor_op::start_processor;
pub use wire_link_op::{
    unwire_link, wire_link, LinkInputDataReaderWrapper, LinkOutputDataWriterWrapper,
};
