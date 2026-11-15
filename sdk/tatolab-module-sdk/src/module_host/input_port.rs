// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An input port Python lent to native code, and the messages read off it.

use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_EMPTY, ModuleHostTableBytes, ModuleHostTableErrorMessage,
    ModuleHostTablePayloadDescriptor, ModuleHostTablePortHandle, ModuleHostTableReadMessage,
    ModuleHostTableSurfaceHandle,
};

use super::{ModuleHost, ModuleHostError, ModuleHostSurface, refuse_unless_ok};

/// An input port Python lent to native code.
#[derive(Debug, Clone)]
pub struct ModuleHostInputPort {
    pub(super) module_host: ModuleHost,
    pub(super) port_name: String,
    pub(super) handle: ModuleHostTablePortHandle,
}

impl ModuleHostInputPort {
    /// The port's declared name.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Take the port's next message, or `None` when its mailbox is empty.
    pub fn read(&self) -> Result<Option<ModuleHostReadMessage>, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut read_message = ModuleHostTableReadMessage::empty();
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `ModuleHostOutputPort::write`.
        let status = unsafe {
            (table.input_port_read)(
                table.node_host,
                self.handle,
                &mut read_message,
                &mut error_message,
            )
        };
        if status == MODULE_HOST_TABLE_STATUS_EMPTY {
            return Ok(None);
        }
        refuse_unless_ok("input_port_read", status, &error_message)?;
        Ok(Some(ModuleHostReadMessage {
            module_host: self.module_host.clone(),
            read_message,
        }))
    }
}

/// One message read off an input port; the payload is handed back to the host
/// when this drops.
#[derive(Debug)]
pub struct ModuleHostReadMessage {
    module_host: ModuleHost,
    read_message: ModuleHostTableReadMessage,
}

// SAFETY: the host owns the payload until release, which any thread may call.
unsafe impl Send for ModuleHostReadMessage {}

impl ModuleHostReadMessage {
    /// The payload exactly as the producer wrote it.
    pub fn payload(&self) -> &[u8] {
        // SAFETY: the host keeps the payload alive until this message drops.
        unsafe {
            ModuleHostTableBytes {
                bytes: self.read_message.payload,
                length: self.read_message.payload_length,
            }
            .as_slice()
        }
    }

    /// The producer's stamp, on the engine's monotonic clock.
    pub fn timestamp_ns(&self) -> i64 {
        self.read_message.timestamp_ns
    }

    /// Whether the message opens a group.
    pub fn starts_group(&self) -> bool {
        self.read_message.starts_group != 0
    }

    /// The messages this one's inbound link lost since the read before it.
    pub fn lost_since_last_read(&self) -> u64 {
        self.read_message.lost_since_last_read
    }

    /// What the payload is: its kind and that kind's body.
    pub fn payload_descriptor(&self) -> Result<ModuleHostTablePayloadDescriptor, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut payload_descriptor = ModuleHostTablePayloadDescriptor::bytes();
        // SAFETY: the host owns the message until this value drops.
        let status = unsafe {
            (table.input_message_payload_descriptor)(
                table.node_host,
                &self.read_message,
                &mut payload_descriptor,
            )
        };
        refuse_unless_ok(
            "input_message_payload_descriptor",
            status,
            &ModuleHostTableErrorMessage::empty(),
        )?;
        Ok(payload_descriptor)
    }

    /// Lend native code the pooled GPU frame this message's Image frame or
    /// GPU pool slot frame names, held against its producer's reuse until the
    /// lent surface drops.
    pub fn lend_pooled_gpu_frame(&self) -> Result<ModuleHostSurface, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut lent_surface = ModuleHostTableSurfaceHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `payload_descriptor`; the slots are live for the call.
        let status = unsafe {
            (table.input_message_lend_pooled_gpu_frame)(
                table.node_host,
                &self.read_message,
                &mut lent_surface,
                &mut error_message,
            )
        };
        refuse_unless_ok(
            "input_message_lend_pooled_gpu_frame",
            status,
            &error_message,
        )?;
        Ok(ModuleHostSurface {
            module_host: self.module_host.clone(),
            handle: lent_surface,
        })
    }
}

impl Drop for ModuleHostReadMessage {
    fn drop(&mut self) {
        let table = &self.module_host.module_host_table;
        // SAFETY: hands back the message this value took, once.
        unsafe { (table.input_message_release)(table.node_host, &mut self.read_message) };
    }
}
