// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration-test fixture: a dlopen'd processor that exercises the
//! `VulkanComputeKernelMethodsVTable` typed binding-method slots
//! end-to-end from cdylib code.
//!
//! Lifecycle:
//!   1. `setup()` — nothing (the kernel is built in `start()` so the
//!      construction error path lands in the same place the
//!      integration test reads).
//!   2. `start()` —
//!      a. Construct a [`ComputeKernelDescriptor`] for the
//!         embedded "output[i] = input[i] * 2" SPIR-V shader and
//!         create the kernel through
//!         `gpu_full_access().create_compute_kernel(...)`. Exercises
//!         the FullAccess vtable's `create_compute_kernel` slot
//!         end-to-end (already covered by EscalateSmokeTest but
//!         re-exercised here for completeness — the kernel return
//!         must be valid for the rest of this test).
//!      b. Acquire input + output `StorageBuffer` handles via
//!         `gpu_limited_access().acquire_storage_buffer(...)`.
//!         HOST_VISIBLE allocations, persistently-mapped pointer
//!         cached on the plugin handle.
//!      c. Populate input through `mapped_ptr()` with `[1, 2, 3,
//!         ..., element_count]`. Pure CPU writes through the
//!         persistently-mapped pointer the LimitedAccess vtable
//!         returned at acquire time.
//!      d. Bind input + output through
//!         `kernel.set_storage_buffer_storage(...)` — exercises
//!         the `set_storage_buffer_storage` vtable slot.
//!      e. Stage push constants via
//!         `kernel.set_push_constants_value(&[element_count])` —
//!         exercises the `set_push_constants` vtable slot.
//!      f. Dispatch the kernel via
//!         `kernel.dispatch(group_count, 1, 1)` — exercises the
//!         `dispatch` vtable slot.
//!      g. Read the output buffer back via its `mapped_ptr()` and
//!         compare each element to the CPU reference (`input[i] *
//!         2`). Any mismatch is a hard fail.
//!      h. Write `OK\n<element_count>` or `ERR:<message>` to the
//!         configured `output_path` so the integration test can
//!         assert the round-trip succeeded.
//!   3. `teardown()` — nothing; the kernel + storage buffers drop
//!      naturally via their plugin-handle Drop impls (which fire
//!      `drop_compute_kernel` and `drop_storage_buffer` on the
//!      respective vtables).
//!
//! What this locks: a regression that breaks any of
//! `create_compute_kernel`, `acquire_storage_buffer`,
//! `set_storage_buffer_storage`, `set_push_constants`, or
//! `dispatch` at the cdylib boundary surfaces here as either:
//!   - A missing output file (cdylib's `start()` panicked at the
//!     FFI boundary and `run_host_extern_c` swallowed the panic).
//!   - `ERR:<message>` in the file (one of the vtable dispatches
//!     returned an error code).
//!   - The output buffer's contents disagreeing with the CPU
//!     reference (the GPU dispatch ran but produced wrong output,
//!     e.g. a binding-handle mismatch routed the wrong buffer
//!     pointer to the wrong slot).

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{ComputeBindingKind, ComputeBindingSpec, ComputeKernelDescriptor};

/// SPIR-V for the `output[i] = input[i] * 2` reference kernel.
/// Compiled from `shaders/cpu_ref_doubler.comp` by this crate's
/// `build.rs` and staged at `OUT_DIR/cpu_ref_doubler.spv`.
const CPU_REF_DOUBLER_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cpu_ref_doubler.spv"));

const CPU_REF_DOUBLER_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec {
        binding: 0,
        kind: ComputeBindingKind::StorageBuffer,
    },
    ComputeBindingSpec {
        binding: 1,
        kind: ComputeBindingKind::StorageBuffer,
    },
];

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/ComputeKernelTestProcessor@1.0.0",
    execution = manual,
    config = crate::_generated_::ComputeKernelTestProcessorConfig,
)]
pub struct ComputeKernelTest {}

impl ManualProcessor for ComputeKernelTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        let element_count = self.config.element_count;

        let outcome = run_compute_kernel_round_trip(ctx, element_count);

        let line = match outcome {
            Ok(()) => format!("OK\n{element_count}"),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line)
            .map_err(|e| Error::Runtime(format!("ComputeKernelTest: write {output_path}: {e}")))?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn run_compute_kernel_round_trip(
    ctx: &RuntimeContextFullAccess<'_>,
    element_count: u32,
) -> Result<()> {
    if element_count == 0 {
        return Err(Error::Runtime(
            "ComputeKernelTest: element_count must be > 0".into(),
        ));
    }

    let gpu_limited = ctx.gpu_limited_access();

    // Acquire input + output storage buffers up-front through the
    // LimitedAccess vtable. HOST_VISIBLE allocations — persistently-
    // mapped pointer is cached on the plugin handle, no escalation
    // required to read/write them.
    let byte_size = (element_count as u64) * (std::mem::size_of::<u32>() as u64);
    let input = gpu_limited
        .acquire_storage_buffer(byte_size)
        .map_err(|e| Error::Runtime(format!("acquire input storage_buffer: {e}")))?;
    let output = gpu_limited
        .acquire_storage_buffer(byte_size)
        .map_err(|e| Error::Runtime(format!("acquire output storage_buffer: {e}")))?;

    // Kernel construction is FullAccess-privileged (touches
    // descriptor pools + pipeline cache + queue). Manual-mode
    // start() takes FullAccess directly; the engine wraps cdylib
    // lifecycle dispatch in `with_cdylib_scope` (#1075), so
    // `ctx.gpu_full_access()` is `ScopeToken`-flavored and
    // dispatches through the FullAccess vtable transparently.
    // Same coverage as the pre-#1075 escalate path; the wrap is the
    // engine-side replacement for the explicit `.escalate(|full|...)`.
    // The kernel plugin handle is valid for the rest of this fn —
    // its Clone/Drop route through the host's FullAccess parent
    // vtable (#918's Phase D shape) and its per-method dispatch
    // routes through the VulkanComputeKernelMethodsVTable installed
    // in `install_host_services` (#907 PR 2/5 + #963's v3 method
    // slots).
    let full = ctx.gpu_full_access();
    let kernel = full
        .create_compute_kernel(&ComputeKernelDescriptor {
            label: "cpu_ref_doubler",
            spv: CPU_REF_DOUBLER_SPV,
            bindings: CPU_REF_DOUBLER_BINDINGS,
            push_constant_size: std::mem::size_of::<u32>() as u32,
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
        let expected = ((i as u32) + 1) * 2u32;
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
    _ctx: &RuntimeContextFullAccess<'_>,
    _element_count: u32,
) -> Result<()> {
    Err(Error::Runtime(
        "ComputeKernelTest: compute kernel dispatch is Linux-only today".into(),
    ))
}
