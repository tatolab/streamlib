// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An output port Python lent to native code, and the writes native code makes on it.

use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_TIMESTAMP_NOW, ModuleHostTableBytes, ModuleHostTableErrorMessage,
    ModuleHostTablePayloadDescriptor, ModuleHostTablePortHandle, ModuleHostTableUtf8,
};

use super::{ModuleHost, ModuleHostError, refuse_unless_ok};

/// An output port Python lent to native code.
#[derive(Debug, Clone)]
pub struct ModuleHostOutputPort {
    pub(super) module_host: ModuleHost,
    pub(super) port_name: String,
    pub(super) handle: ModuleHostTablePortHandle,
}

impl ModuleHostOutputPort {
    /// The port's declared name.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Publish `payload` to every downstream link, stamped `timestamp_ns` or,
    /// with `None`, its stream's clock now.
    pub fn write(
        &self,
        payload: &[u8],
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self.module_host`; the
        // payload and the message buffer are live for the call.
        let status = unsafe {
            (table.output_port_write)(
                table.node_host,
                self.handle,
                ModuleHostTableBytes::of(payload),
                timestamp_ns.unwrap_or(MODULE_HOST_TABLE_TIMESTAMP_NOW),
                u32::from(starts_group),
                &mut error_message,
            )
        };
        refuse_unless_ok("output_port_write", status, &error_message)
    }

    /// Publish `payload` described by `payload_descriptor` to every
    /// downstream link, stamped as [`Self::write`] stamps.
    pub fn write_described(
        &self,
        payload: &[u8],
        timestamp_ns: Option<i64>,
        starts_group: bool,
        payload_descriptor: &ModuleHostTablePayloadDescriptor,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `write`; the descriptor is live for the call.
        let status = unsafe {
            (table.output_port_write_described)(
                table.node_host,
                self.handle,
                ModuleHostTableBytes::of(payload),
                timestamp_ns.unwrap_or(MODULE_HOST_TABLE_TIMESTAMP_NOW),
                u32::from(starts_group),
                payload_descriptor,
                &mut error_message,
            )
        };
        refuse_unless_ok("output_port_write_described", status, &error_message)
    }

    /// Declare `given_up_group_count` groups given up before this port's next
    /// write; its readers count them as lost since their last read.
    pub fn declare_groups_given_up_before_the_next_write(
        &self,
        given_up_group_count: u64,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `write`.
        let status = unsafe {
            (table.output_port_declare_groups_given_up_before_the_next_write)(
                table.node_host,
                self.handle,
                given_up_group_count,
                &mut error_message,
            )
        };
        refuse_unless_ok(
            "output_port_declare_groups_given_up_before_the_next_write",
            status,
            &error_message,
        )
    }

    /// Replace the port's stream facts: `family` is `image` or `packets`, the
    /// port's own, and `stream_facts_json` the facts' fields as one JSON object.
    pub fn set_stream_facts(
        &self,
        family: &str,
        stream_facts_json: &str,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `write`.
        let status = unsafe {
            (table.output_port_set_stream_facts)(
                table.node_host,
                self.handle,
                ModuleHostTableUtf8::of(family),
                ModuleHostTableUtf8::of(stream_facts_json),
                &mut error_message,
            )
        };
        refuse_unless_ok("output_port_set_stream_facts", status, &error_message)
    }
}
