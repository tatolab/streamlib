// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Safe Rust over the table's shared native service leases: a node's hold on
//! a native service the runtime supervises for the whole machine, such as the
//! browser host, and the socket the node reaches it on.

use std::path::{Path, PathBuf};

use crate::module_host::{ModuleHost, ModuleHostError, refuse_unless_ok};
use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_EMPTY, MODULE_HOST_TABLE_STATUS_ENDED, MODULE_HOST_TABLE_STATUS_OK,
    ModuleHostTableErrorMessage, ModuleHostTableSharedNativeServiceLease,
    ModuleHostTableSharedNativeServiceLeaseHandle, ModuleHostTableUtf8,
};

/// Whether a leased service is there to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleHostSharedNativeServiceState {
    Running,
    /// The service ended and the runtime starts it again after a backoff.
    StartingAgain,
    /// The service failed for good, and why.
    Failed(String),
}

/// A node's lease on a shared native service; dropping it gives it back.
#[derive(Debug)]
pub struct ModuleHostSharedNativeServiceLease {
    module_host: ModuleHost,
    lease: ModuleHostTableSharedNativeServiceLeaseHandle,
    endpoint_socket_path: PathBuf,
    tenant_stream_id: String,
}

impl ModuleHost {
    /// Lease the runtime's shared native service `service_name`, the runtime
    /// starting it when no node holds a lease on it.
    pub fn lease_shared_native_service(
        &self,
        service_name: &str,
    ) -> Result<ModuleHostSharedNativeServiceLease, ModuleHostError> {
        let mut granted_lease = ModuleHostTableSharedNativeServiceLease {
            lease: ModuleHostTableSharedNativeServiceLeaseHandle { value: 0 },
            endpoint_socket_path: ModuleHostTableUtf8::of(""),
            tenant_stream_id: ModuleHostTableUtf8::of(""),
        };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: `self` retains the node host; every pointer is live for the
        // call.
        let status = unsafe {
            (self.module_host_table.shared_native_service_lease_acquire)(
                self.module_host_table.node_host,
                ModuleHostTableUtf8::of(service_name),
                &mut granted_lease,
                &mut error_message,
            )
        };
        refuse_unless_ok("shared_native_service_lease_acquire", status, &error_message)?;
        // SAFETY: the host keeps a granted lease's text alive until it is
        // released, and it is copied out here before anything releases it.
        let (endpoint_socket_path, tenant_stream_id) = unsafe {
            (
                granted_lease
                    .endpoint_socket_path
                    .as_str()
                    .map(PathBuf::from),
                granted_lease.tenant_stream_id.as_str().map(str::to_string),
            )
        };
        let mut lease = ModuleHostSharedNativeServiceLease {
            module_host: self.clone(),
            lease: granted_lease.lease,
            endpoint_socket_path: PathBuf::new(),
            tenant_stream_id: String::new(),
        };
        let not_text = |_| ModuleHostError::VerbRefused {
            verb: "shared_native_service_lease_acquire",
            refusal: "the host granted a lease whose text is not UTF-8".to_string(),
        };
        lease.endpoint_socket_path = endpoint_socket_path.map_err(not_text)?;
        lease.tenant_stream_id = tenant_stream_id.map_err(not_text)?;
        Ok(lease)
    }
}

impl ModuleHostSharedNativeServiceLease {
    /// The Unix socket the service accepts this node's connections on.
    pub fn endpoint_socket_path(&self) -> &Path {
        &self.endpoint_socket_path
    }

    /// The stream the lease was granted to.
    pub fn tenant_stream_id(&self) -> &str {
        &self.tenant_stream_id
    }

    /// Whether the service is running, starting again, or failed for good.
    pub fn state(&self) -> Result<ModuleHostSharedNativeServiceState, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the lease is this host's until drop.
        let status = unsafe {
            (table.shared_native_service_lease_state)(
                table.node_host,
                self.lease,
                &mut error_message,
            )
        };
        match status {
            MODULE_HOST_TABLE_STATUS_OK => Ok(ModuleHostSharedNativeServiceState::Running),
            MODULE_HOST_TABLE_STATUS_EMPTY => {
                Ok(ModuleHostSharedNativeServiceState::StartingAgain)
            }
            MODULE_HOST_TABLE_STATUS_ENDED => Ok(ModuleHostSharedNativeServiceState::Failed(
                error_message.message(),
            )),
            refused => {
                refuse_unless_ok("shared_native_service_lease_state", refused, &error_message)?;
                Ok(ModuleHostSharedNativeServiceState::Running)
            }
        }
    }
}

impl Drop for ModuleHostSharedNativeServiceLease {
    fn drop(&mut self) {
        let table = &self.module_host.module_host_table;
        // SAFETY: gives back the lease this value holds, once.
        unsafe { (table.shared_native_service_lease_release)(table.node_host, self.lease) };
    }
}
