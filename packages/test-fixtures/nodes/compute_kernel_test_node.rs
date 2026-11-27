// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Integration-test fixture that exercises the compute-kernel typed
//! binding methods end-to-end.
//!
//! Its one computed value, built from `element_count`:
//!   a. Acquires input + output `StorageBuffer` handles via
//!      `acquire_storage_buffer(...)` on the limited access with a
//!      byte-shaped `TensorStorageBufferLayout`: HOST_VISIBLE
//!      allocations, the persistently-mapped pointer cached on the
//!      handle.
//!   b. Creates the kernel for the embedded "output[i] = input[i] * 2"
//!      SPIR-V shader through `escalate(|full| full.create_compute_kernel(...))`.
//!   c. Populates input through `mapped_ptr()` with `[1, 2, 3, ...,
//!      element_count]`.
//!   d. Binds input + output through `kernel.set_storage_buffer_storage(...)`,
//!      stages push constants via `kernel.set_push_constants_value(&element_count)`
//!      and dispatches via `kernel.dispatch(group_count, 1, 1)`.
//!   e. Reads the output buffer back via its `mapped_ptr()` and compares
//!      each element to the CPU reference (`input[i] * 2`). Any mismatch
//!      is a hard fail.
//!   f. Writes `OK\n<element_count>` or `ERR:<message>` to the file its
//!      `output_path` names so the integration test can assert the
//!      round-trip succeeded.
//!
//! The kernel and storage buffers drop with the build that made them.
//!
//! What this locks: a regression that breaks any of
//! `create_compute_kernel`, `acquire_storage_buffer`,
//! `set_storage_buffer_storage`, `set_push_constants`, or
//! `dispatch` surfaces here as either:
//!   - A missing output file (the build panicked).
//!   - `ERR:<message>` in the file (one of the calls returned an
//!     error code).
//!   - The output buffer's contents disagreeing with the CPU
//!     reference (the GPU dispatch ran but produced wrong output,
//!     e.g. a binding-handle mismatch routed the wrong buffer
//!     pointer to the wrong slot).

use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::nodes::{Input, computed};
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::{
    ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor, TensorStorageBufferLayout,
};

use crate::result_written_to_its_file::the_result_written_to_its_file;

/// SPIR-V for the `output[i] = input[i] * 2` reference kernel.
/// Compiled from `shaders/cpu_ref_doubler.slang` by this crate's
/// `build.rs` and staged at `OUT_DIR/cpu_ref_doubler.spv`.
#[cfg(target_os = "linux")]
const CPU_REF_DOUBLER_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cpu_ref_doubler.spv"));

#[cfg(target_os = "linux")]
const CPU_REF_DOUBLER_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec {
        binding: 0,
        kind: ComputeBindingKind::StorageBuffer,
        name: None,
        sampler: None,
    },
    ComputeBindingSpec {
        binding: 1,
        kind: ComputeBindingKind::StorageBuffer,
        name: None,
        sampler: None,
    },
];

#[streamlib::sdk::node(
    execution = manual,
    input("element_count", description = "The number of elements in the buffer the kernel doubles.", value = u32, default = 0),
    input("output_path", description = "The file the node writes its result to.", value = String, default = String::new()),
)]
#[meta(
    description = "Compute-kernel CPU-reference integration test fixture — in a computed value read from its element_count, allocates input + output storage buffers via LimitedAccess, creates a kernel through escalate, dispatches output[i] = input[i] * 2, asserts CPU-reference match, and writes the result to the file its output_path names"
)]
pub fn compute_kernel_test(
    element_count: Input<u32>,
    output_path: Input<String>,
    ctx: &RuntimeContextFullAccess<'_>,
) {
    let gpu = ctx.gpu_limited_access().clone();
    computed(move || {
        let element_count = *element_count.get();
        let outcome = run_compute_kernel_round_trip(&gpu, element_count)
            .map(|()| format!("\n{element_count}"));
        the_result_written_to_its_file("ComputeKernelTest", &output_path.get(), &outcome)
    });
}

#[cfg(target_os = "linux")]
fn run_compute_kernel_round_trip(gpu: &GpuContextLimitedAccess, element_count: u32) -> Result<()> {
    if element_count == 0 {
        return Err(Error::Runtime(
            "ComputeKernelTest: element_count must be > 0".into(),
        ));
    }

    // Acquire input + output storage buffers up-front through
    // LimitedAccess. HOST_VISIBLE allocations — the persistently-
    // mapped pointer is cached on the handle, no escalation
    // required to read/write them.
    let byte_size = (element_count as u64) * (std::mem::size_of::<u32>() as u64);
    let input = gpu
        .acquire_storage_buffer(&TensorStorageBufferLayout::of_bytes(byte_size)?)
        .map_err(|e| Error::Runtime(format!("acquire input storage_buffer: {e}")))?;
    let output = gpu
        .acquire_storage_buffer(&TensorStorageBufferLayout::of_bytes(byte_size)?)
        .map_err(|e| Error::Runtime(format!("acquire output storage_buffer: {e}")))?;

    // Kernel construction is FullAccess-privileged (touches descriptor pools + pipeline cache +
    // queue), so the build escalates for it. The kernel handle is valid for the rest of this fn;
    // its Clone/Drop manage the underlying Arc's strong count.
    let kernel = gpu
        .escalate(|full| {
            full.create_compute_kernel(&ComputeKernelDescriptor {
                label: "cpu_ref_doubler",
                entry_point: "main",
                spv: CPU_REF_DOUBLER_SPV,
                bindings: CPU_REF_DOUBLER_BINDINGS,
                push_constant_size: std::mem::size_of::<u32>() as u32,
                specialization_constants: &[],
            })
        })
        .map_err(|e| Error::Runtime(format!("create_compute_kernel: {e}")))?;

    if input.mapped_ptr().is_null() {
        return Err(Error::Runtime(
            "ComputeKernelTest: input storage_buffer mapped_ptr is null".into(),
        ));
    }
    if output.mapped_ptr().is_null() {
        return Err(Error::Runtime(
            "ComputeKernelTest: output storage_buffer mapped_ptr is null".into(),
        ));
    }

    // Populate input with `[1, 2, ..., element_count]` so the CPU
    // reference (input[i] * 2) is non-trivial — a zero-filled buffer
    // would let an "output never written" regression pass.
    {
        let input_slice = unsafe {
            std::slice::from_raw_parts_mut(input.mapped_ptr() as *mut u32, element_count as usize)
        };
        for (i, slot) in input_slice.iter_mut().enumerate() {
            *slot = (i as u32) + 1;
        }
    }
    // Pre-fill output with a sentinel so an "output buffer never
    // bound / never written" failure is distinguishable from a real
    // dispatch result. The sentinel must be one the CPU reference
    // would never produce — input range is [1, count] so doubled
    // range is [2, 2*count]; 0xDEADBEEF is well outside.
    {
        let output_slice = unsafe {
            std::slice::from_raw_parts_mut(output.mapped_ptr() as *mut u32, element_count as usize)
        };
        for slot in output_slice.iter_mut() {
            *slot = 0xDEADBEEFu32;
        }
    }

    kernel
        .set_storage_buffer_storage(0, &input)
        .map_err(|e| Error::Runtime(format!("set_storage_buffer_storage (binding 0): {e}")))?;
    kernel
        .set_storage_buffer_storage(1, &output)
        .map_err(|e| Error::Runtime(format!("set_storage_buffer_storage (binding 1): {e}")))?;

    kernel
        .set_push_constants_value(&element_count)
        .map_err(|e| Error::Runtime(format!("set_push_constants: {e}")))?;

    let group_count = element_count.div_ceil(64);
    kernel
        .dispatch(group_count, 1, 1)
        .map_err(|e| Error::Runtime(format!("dispatch: {e}")))?;

    // Compare every element against the trivial CPU reference. Stop
    // at the first mismatch so the error message names the offending
    // index without dumping the entire buffer.
    let output_slice = unsafe {
        std::slice::from_raw_parts(output.mapped_ptr() as *const u32, element_count as usize)
    };
    for i in 0..element_count {
        let expected = (i + 1) * 2u32;
        let observed = output_slice[i as usize];
        if observed != expected {
            return Err(Error::Runtime(format!(
                "ComputeKernelTest: output[{i}] = {observed:#010x}, expected {expected:#010x} (input[{i}] = {})",
                i + 1
            )));
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn run_compute_kernel_round_trip(
    _gpu: &GpuContextLimitedAccess,
    _element_count: u32,
) -> Result<()> {
    Err(Error::Runtime(
        "ComputeKernelTest: compute kernel dispatch is Linux-only today".into(),
    ))
}
