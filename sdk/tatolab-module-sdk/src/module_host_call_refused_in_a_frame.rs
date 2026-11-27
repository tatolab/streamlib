// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Safe Rust over the table's refusal of a module's own call too slow to run every frame: the
//! host refuses it while a frame of the node's runs on the calling thread, as it refuses its own.

use crate::module_host::{ModuleHost, ModuleHostError, refuse_unless_ok};
use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_CALL_CREATES_A_KERNEL, MODULE_HOST_TABLE_CALL_OPENS_A_DEVICE,
    MODULE_HOST_TABLE_CALL_STARTS_A_SESSION, ModuleHostTableErrorMessage, ModuleHostTableUtf8,
};

/// What a module's own call too slow to run every frame does, each refused by its own code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleHostCallTooSlowForAFrame {
    /// The call creates a kernel.
    CreatesAKernel,
    /// The call opens a device.
    OpensADevice,
    /// The call starts a session.
    StartsASession,
}

impl ModuleHostCallTooSlowForAFrame {
    /// What the call does, as the table names it.
    fn on_the_table(self) -> u32 {
        match self {
            Self::CreatesAKernel => MODULE_HOST_TABLE_CALL_CREATES_A_KERNEL,
            Self::OpensADevice => MODULE_HOST_TABLE_CALL_OPENS_A_DEVICE,
            Self::StartsASession => MODULE_HOST_TABLE_CALL_STARTS_A_SESSION,
        }
    }

    /// The code the host's refusal leads with: `kernel_created_in_a_frame`,
    /// `device_opened_in_a_frame` or `session_started_in_a_frame`.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::CreatesAKernel => "kernel_created_in_a_frame",
            Self::OpensADevice => "device_opened_in_a_frame",
            Self::StartsASession => "session_started_in_a_frame",
        }
    }

    /// What the call refused in `refusal` does, read off the code the host leads it with.
    pub(crate) fn leading(refusal: &str) -> Option<Self> {
        [
            Self::CreatesAKernel,
            Self::OpensADevice,
            Self::StartsASession,
        ]
        .into_iter()
        .find(|does| {
            refusal
                .strip_prefix(does.code())
                .is_some_and(|words| words.starts_with(": "))
        })
    }

    /// The name of the Python exception the refusal is raised as.
    #[cfg(feature = "pyo3")]
    pub(crate) fn python_exception_name(self) -> &'static str {
        match self {
            Self::CreatesAKernel => "KernelCreatedInAFrame",
            Self::OpensADevice => "DeviceOpenedInAFrame",
            Self::StartsASession => "SessionStartedInAFrame",
        }
    }
}

impl ModuleHost {
    /// Refuse `call`, which does what `does` names, while a frame of the node's runs on this
    /// thread, in the host's words, which show the fix; `Ok` in the node's body, in a build, and
    /// anywhere else. Ask before the call does its work.
    pub fn refuse_while_a_frame_runs(
        &self,
        does: ModuleHostCallTooSlowForAFrame,
        call: &str,
    ) -> Result<(), ModuleHostError> {
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self`; every pointer is live for the call.
        let status = unsafe {
            (self.module_host_table.call_refused_while_a_frame_runs)(
                self.module_host_table.node_host,
                does.on_the_table(),
                ModuleHostTableUtf8::of(call),
                &mut error_message,
            )
        };
        refuse_unless_ok("call_refused_while_a_frame_runs", status, &error_message)
    }
}
