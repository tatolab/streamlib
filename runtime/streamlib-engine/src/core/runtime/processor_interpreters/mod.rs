// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The interpreter processes processors run in: their launch records, process groups and descriptor
//! limits.

pub(super) mod helper_process_group_registry;
pub(super) mod local_processor_type_registration;
pub(super) mod open_descriptor_soft_limit;
pub(super) mod processor_interpreter_launch_record;
