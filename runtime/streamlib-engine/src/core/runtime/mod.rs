// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod capability_extensions;
mod graph_change_listener;
mod local_processor_type_registration;
mod operations;
mod operations_runtime;
pub(crate) use operations_runtime::mark_this_thread_as_a_processor_execution_thread;
#[allow(clippy::module_inception)]
mod runtime;
mod runtime_shutdown_request;
mod runtime_unique_id;
mod status;
mod streamlib_runtime_directory;
mod surface_image_exchange;
mod tap;

pub use capability_extensions::{LoadedCapabilityExtension, LoadedCapabilityExtensionRegistry};
pub use operations::{BoxFuture, RuntimeOperations};
pub use runtime::Runner;
#[cfg(test)]
pub(crate) use runtime_shutdown_request::RuntimeShutdownRequestLatchClearedOnDrop;
pub use runtime_shutdown_request::{
    RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL, is_runtime_shutdown_requested,
    request_runtime_shutdown, take_runtime_shutdown_request_latch,
};
pub use runtime_unique_id::RuntimeUniqueId;
pub use status::RuntimeStatus;
pub(crate) use streamlib_runtime_directory::current_process_uid;
pub use streamlib_runtime_directory::StreamlibRuntimeDirectory;
pub use surface_image_exchange::ExchangedPublishedSurfaceFramePngImage;
pub use tap::TapSubscription;
