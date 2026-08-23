// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::concurrent_read_timeline_signals`
//! — concurrent readers of one surface each get their own
//! `produce_done` value, and their copies never outlive the acquire
//! that submitted them.
//!
//! Two readers that snapshot the same per-surface counter derive the
//! same next value. The first submit signals it; the second is
//! `VUID-VkSubmitInfo2-semaphore-03882` ("signal value must be greater
//! than current timeline semaphore value") — and because the value is
//! already signaled, the second reader's post-copy wait returns while
//! its own copy is still in flight, so teardown destroys the source
//! image, a staging buffer and the timeline out from under a live
//! submit.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use streamlib::sdk::engine::host_rhi::{HostMarker, HostVulkanDevice};
use streamlib_adapter_cpu_readback::{
    CpuReadbackCopyTrigger, CpuReadbackTriggerContext, InProcessCpuReadbackCopyTrigger,
};
use streamlib_surface_adapter::{AdapterError, StreamlibSurface, SurfaceAdapter};

use crate::common::HostFixture;

const SURFACE_ID_DISTINCT_VALUES: u64 = 0x_1914_0001;
const SURFACE_ID_VALIDATION_CLEAN: u64 = 0x_1914_0002;

/// Enough readers that any interleaving in which two of them snapshot
/// the surface counter before either commits shows up as a duplicate.
const CONCURRENT_READER_THREAD_COUNT: usize = 8;

/// Wraps the in-process trigger and records the timeline value the
/// adapter handed each copy, in the order the copies were submitted.
struct SignalValueRecordingCopyTrigger {
    in_process: Arc<InProcessCpuReadbackCopyTrigger<HostVulkanDevice>>,
    suggested_signal_values_in_submit_order: Mutex<Vec<u64>>,
}

impl SignalValueRecordingCopyTrigger {
    fn new(in_process: Arc<InProcessCpuReadbackCopyTrigger<HostVulkanDevice>>) -> Self {
        Self {
            in_process,
            suggested_signal_values_in_submit_order: Mutex::new(Vec::new()),
        }
    }

    fn recorded_signal_values(&self) -> Vec<u64> {
        self.suggested_signal_values_in_submit_order
            .lock()
            .expect("recording trigger mutex poisoned")
            .clone()
    }
}

impl CpuReadbackCopyTrigger<HostMarker> for SignalValueRecordingCopyTrigger {
    fn run_copy_image_to_buffer(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, HostMarker>,
    ) -> Result<u64, AdapterError> {
        self.suggested_signal_values_in_submit_order
            .lock()
            .expect("recording trigger mutex poisoned")
            .push(ctx.suggested_signal_value);
        self.in_process.run_copy_image_to_buffer(ctx)
    }

    fn run_copy_buffer_to_image(
        &self,
        ctx: &CpuReadbackTriggerContext<'_, HostMarker>,
    ) -> Result<u64, AdapterError> {
        self.suggested_signal_values_in_submit_order
            .lock()
            .expect("recording trigger mutex poisoned")
            .push(ctx.suggested_signal_value);
        self.in_process.run_copy_buffer_to_image(ctx)
    }
}

/// Release `CONCURRENT_READER_THREAD_COUNT` threads onto one surface at
/// once and hold every read guard until the last thread has one, so the
/// acquires genuinely overlap rather than queueing.
fn acquire_reads_concurrently(fixture: &HostFixture, surface: &StreamlibSurface) {
    let all_threads_ready = Barrier::new(CONCURRENT_READER_THREAD_COUNT);
    thread::scope(|scope| {
        for reader_index in 0..CONCURRENT_READER_THREAD_COUNT {
            let all_threads_ready = &all_threads_ready;
            let adapter = &fixture.adapter;
            scope.spawn(move || {
                all_threads_ready.wait();
                let guard = adapter
                    .acquire_read(surface)
                    .unwrap_or_else(|e| panic!("reader {reader_index} acquire_read: {e:?}"));
                drop(guard);
            });
        }
    });
}

#[test]
fn concurrent_read_acquires_never_reuse_a_produce_done_signal_value() {
    let fixture = HostFixture::try_new_wrapping_copy_trigger(|in_process| {
        let recording = Arc::new(SignalValueRecordingCopyTrigger::new(in_process));
        (
            Arc::clone(&recording) as Arc<dyn CpuReadbackCopyTrigger<HostMarker>>,
            recording,
        )
    });
    let Some((fixture, recording_trigger)) = fixture else {
        println!(
            "concurrent_read_acquires_never_reuse_a_produce_done_signal_value: \
             no GPU — skipping"
        );
        return;
    };

    let surface = fixture.register_surface(SURFACE_ID_DISTINCT_VALUES, 64, 64);
    acquire_reads_concurrently(&fixture, &surface);

    let signal_values = recording_trigger.recorded_signal_values();
    assert_eq!(
        signal_values.len(),
        CONCURRENT_READER_THREAD_COUNT,
        "every reader submits exactly one copy: {signal_values:?}"
    );
    assert!(
        signal_values.windows(2).all(|pair| pair[1] > pair[0]),
        "each copy must signal a value strictly above the one before it, \
         or the later submit is rejected and its wait is satisfied by the \
         earlier copy: {signal_values:?}"
    );
}

#[test]
fn a_concurrent_read_burst_and_its_teardown_raise_no_validation_finding() {
    let Some(fixture) = HostFixture::try_new() else {
        println!(
            "a_concurrent_read_burst_and_its_teardown_raise_no_validation_finding: \
             no GPU — skipping"
        );
        return;
    };
    // The shared `GpuContext` outlives the fixture, so this handle stays
    // valid across the teardown the destroy-while-in-use findings ride.
    let device = Arc::clone(fixture.adapter.device());
    let counts_before = device.validation_layer_message_counts();
    if counts_before.is_none() {
        println!(
            "a_concurrent_read_burst_and_its_teardown_raise_no_validation_finding: \
             no validation messenger installed — skipping. Re-run with \
             STREAMLIB_VULKAN_VALIDATION=1 and VK_LAYER_KHRONOS_validation present."
        );
        return;
    }

    let surface = fixture.register_surface(SURFACE_ID_VALIDATION_CLEAN, 64, 64);
    acquire_reads_concurrently(&fixture, &surface);
    assert_eq!(
        device.validation_layer_message_counts(),
        counts_before,
        "concurrent read acquires must raise no validation finding"
    );

    drop(fixture);
    assert_eq!(
        device.validation_layer_message_counts(),
        counts_before,
        "teardown must not destroy an image, buffer or timeline a submit still references"
    );
}
