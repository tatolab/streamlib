// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Negative-test evidence for #572. This file is NEVER referenced from the
// cdylib's module tree, so the cdylib still builds; but boundary-grep
// walks every `*.rs` under `libs/` and should emit two violations here
// (Check 2 `use vulkanalia` + Check 4 privileged `.allocate_memory(`).
// Reverted in the immediately-following commit.

use vulkanalia::vk;

#[allow(dead_code)]
fn _planted_break(device: &vk::Device) {
    unsafe {
        let _ = device.allocate_memory(&vk::MemoryAllocateInfo::default(), None);
    }
}
