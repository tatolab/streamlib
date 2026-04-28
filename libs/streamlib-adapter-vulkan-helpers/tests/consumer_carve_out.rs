// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Carve-out semantic tests for `ConsumerVulkanDevice`.
//!
//! Phase 1 (#561) only landed thin "constructs + exposes queue" tests.
//! Phase 2 (#560) lands the cdylib swap to `ConsumerVulkanDevice`, so
//! the consumer device starts running in production. These tests
//! cover the carve-out paths that aren't already covered by the
//! adapter-vulkan host↔subprocess round-trip integration tests
//! (`streamlib-adapter-vulkan/tests/round_trip_*` — those exercise
//! the full DMA-BUF import + bind + map flow against a real host
//! allocation):
//!
//! - **Concurrent submit serialization** — threads issuing
//!   `submit_to_queue` against the same `VkQueue` with real
//!   `vkCmdFillBuffer` work, per-submit fences, and per-thread
//!   readback verification. Without the per-queue mutex, concurrent
//!   `vkQueueSubmit2` is UB; the failure mode that matters in
//!   practice is a lost / corrupted submit, which the readback would
//!   catch.
//! - **Leak-on-drop tracing** — `Drop` emits `tracing::warn!` when
//!   the device is dropped with live DMA-BUF imports. Captured here
//!   via a custom `tracing_subscriber` layer that records warn-level
//!   events. Real DMA-BUF import: host allocates an exportable
//!   buffer, exports the fd, consumer imports — drop the consumer
//!   without freeing.
//!
//! All tests skip gracefully when no GPU / no Vulkan loader is
//! available, matching `consumer_vulkan_device.rs::tests`.

#![cfg(target_os = "linux")]

use std::sync::{Arc, Mutex};

use serial_test::serial;
use streamlib::host_rhi::HostVulkanDevice;
use streamlib_consumer_rhi::{ConsumerVulkanDevice, VulkanRhiDevice};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

fn try_consumer() -> Option<Arc<ConsumerVulkanDevice>> {
    match ConsumerVulkanDevice::new() {
        Ok(d) => Some(Arc::new(d)),
        Err(e) => {
            println!("skip: ConsumerVulkanDevice::new failed: {e}");
            None
        }
    }
}

fn try_host() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(d) => Some(Arc::new(d)),
        Err(e) => {
            println!("skip: HostVulkanDevice::new failed: {e}");
            None
        }
    }
}

/// Allocate a HOST_VISIBLE+HOST_COHERENT buffer + memory + map on the
/// consumer device for a single thread's serialization test slot.
/// Returns the buffer, memory, mapped pointer, and the byte count.
fn alloc_consumer_host_visible(
    consumer: &Arc<ConsumerVulkanDevice>,
    size: vk::DeviceSize,
) -> (vk::Buffer, vk::DeviceMemory, *mut u8) {
    let device = consumer.device();
    let buffer_info = vk::BufferCreateInfo::builder()
        .size(size)
        .usage(vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let buffer = unsafe { device.create_buffer(&buffer_info, None) }.expect("create_buffer");
    let req = unsafe { device.get_buffer_memory_requirements(buffer) };

    // Find a HOST_VISIBLE | HOST_COHERENT memory type for this buffer.
    let mem_props = unsafe {
        consumer
            .instance()
            .get_physical_device_memory_properties(consumer.physical_device())
    };
    let needed = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mut chosen: Option<u32> = None;
    for i in 0..mem_props.memory_type_count {
        if (req.memory_type_bits & (1 << i)) != 0
            && mem_props.memory_types[i as usize]
                .property_flags
                .contains(needed)
        {
            chosen = Some(i);
            break;
        }
    }
    let mem_type_index = chosen.expect("no HOST_VISIBLE|HOST_COHERENT memory type");

    let alloc_info = vk::MemoryAllocateInfo::builder()
        .allocation_size(req.size)
        .memory_type_index(mem_type_index);
    let memory = unsafe { device.allocate_memory(&alloc_info, None) }.expect("allocate_memory");
    unsafe { device.bind_buffer_memory(buffer, memory, 0) }.expect("bind_buffer_memory");
    let ptr = unsafe { device.map_memory(memory, 0, req.size, vk::MemoryMapFlags::empty()) }
        .expect("map_memory") as *mut u8;
    (buffer, memory, ptr)
}

fn drop_consumer_alloc(
    consumer: &Arc<ConsumerVulkanDevice>,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
) {
    let device = consumer.device();
    unsafe { device.unmap_memory(memory) };
    unsafe { device.destroy_buffer(buffer, None) };
    unsafe { device.free_memory(memory, None) };
}

#[test]
#[serial]
fn consumer_device_concurrent_submit_serializes_real_work() {
    // Submits real `vkCmdFillBuffer` work from N threads. Each thread
    // owns a slot in a HOST_VISIBLE buffer and writes a thread-unique
    // 32-bit pattern into it. After all threads finish, the test reads
    // back every slot and asserts the pattern survived. A submit
    // dropped or corrupted by a missing per-queue mutex would leave a
    // slot zeroed (or stale), which the readback catches.
    //
    // (A truly UB-on-removed-mutex regression on common drivers may
    // still happen to "work" — but the test bar here is "all
    // submitted work observably committed", which is meaningful.)
    let consumer = match try_consumer() {
        Some(d) => d,
        None => return,
    };
    let device = consumer.device();
    let queue = consumer.queue();
    let qfi = consumer.queue_family_index();

    // One command pool per thread — vkCmdPool requires external sync,
    // so we don't share. The queue is what the trait mutex protects.
    const N_THREADS: u32 = 4;
    const SLOT_BYTES: vk::DeviceSize = 64;
    const TOTAL_BYTES: vk::DeviceSize = SLOT_BYTES * N_THREADS as vk::DeviceSize;

    // Single shared HOST_VISIBLE staging buffer; threads write to
    // disjoint slots so there's no contention on the bytes themselves
    // — only on the queue submission.
    let (buffer, memory, ptr) = alloc_consumer_host_visible(&consumer, TOTAL_BYTES);
    // Pre-zero so we can detect a missing submit.
    unsafe { std::ptr::write_bytes(ptr, 0, TOTAL_BYTES as usize) };

    let mut handles = Vec::with_capacity(N_THREADS as usize);
    for tid in 0..N_THREADS {
        let consumer_t = Arc::clone(&consumer);
        handles.push(std::thread::spawn(move || -> Result<(), String> {
            let dev = consumer_t.device();
            let pool_info = vk::CommandPoolCreateInfo::builder()
                .queue_family_index(qfi)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT);
            let pool = unsafe { dev.create_command_pool(&pool_info, None) }
                .map_err(|e| format!("create_command_pool: {e}"))?;
            let alloc = vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = unsafe { dev.allocate_command_buffers(&alloc) }
                .map_err(|e| format!("allocate_command_buffers: {e}"))?[0];
            let begin = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            unsafe { dev.begin_command_buffer(cmd, &begin) }
                .map_err(|e| format!("begin: {e}"))?;
            let pattern: u32 = 0xCAFE_0000 | tid;
            unsafe {
                dev.cmd_fill_buffer(
                    cmd,
                    buffer,
                    (tid as vk::DeviceSize) * SLOT_BYTES,
                    SLOT_BYTES,
                    pattern,
                );
            }
            unsafe { dev.end_command_buffer(cmd) }.map_err(|e| format!("end: {e}"))?;
            let fence = unsafe {
                dev.create_fence(&vk::FenceCreateInfo::builder(), None)
            }
            .map_err(|e| format!("create_fence: {e}"))?;
            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(cmd)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder().command_buffer_infos(&cmd_infos).build();
            let submits = [submit];
            unsafe {
                <ConsumerVulkanDevice as VulkanRhiDevice>::submit_to_queue(
                    &consumer_t,
                    queue,
                    &submits,
                    fence,
                )
                .map_err(|e| format!("submit_to_queue (tid={tid}): {e}"))?;
            }
            unsafe { dev.wait_for_fences(&[fence], true, u64::MAX) }
                .map_err(|e| format!("wait_for_fences: {e}"))?;
            unsafe { dev.destroy_fence(fence, None) };
            unsafe { dev.destroy_command_pool(pool, None) };
            Ok(())
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        h.join()
            .unwrap_or_else(|_| panic!("thread {i} panicked"))
            .unwrap_or_else(|e| panic!("thread {i}: {e}"));
    }

    // Readback: every slot must hold the thread's pattern.
    for tid in 0..N_THREADS {
        let pattern: u32 = 0xCAFE_0000 | tid;
        let offset = (tid as usize) * SLOT_BYTES as usize;
        for word in 0..(SLOT_BYTES as usize / 4) {
            let observed = unsafe {
                std::ptr::read_unaligned::<u32>(ptr.add(offset + word * 4) as *const u32)
            };
            assert_eq!(
                observed, pattern,
                "tid={tid} word={word}: submit went missing or got reordered against another thread"
            );
        }
    }

    drop_consumer_alloc(&consumer, buffer, memory);
    unsafe {
        let _ = device.queue_wait_idle(queue);
    }
}

#[test]
#[serial]
fn consumer_device_drops_silently_when_no_imports() {
    let consumer = match try_consumer() {
        Some(d) => d,
        None => return,
    };
    assert_eq!(
        consumer.live_import_allocation_count(),
        0,
        "fresh ConsumerVulkanDevice has zero live imports"
    );
    drop(consumer);
}

#[test]
#[serial]
fn consumer_device_implements_vulkan_rhi_device_trait() {
    let consumer = match try_consumer() {
        Some(d) => d,
        None => return,
    };
    fn assert_consumer<D: VulkanRhiDevice>(_: &D) {}
    assert_consumer(&*consumer);

    // The trait surface adapter-vulkan + raw_handles depend on:
    let _instance: &vulkanalia::Instance = consumer.instance();
    let _physical: vk::PhysicalDevice = consumer.physical_device();
    let _device: &vulkanalia::Device = consumer.device();
    let _queue: vk::Queue = consumer.queue();
    let _qfi: u32 = consumer.queue_family_index();
}

// ---------- Leak-on-drop tracing assertion ----------

/// `tracing_subscriber::Layer` that captures warn-level event
/// messages into a shared buffer. Used by
/// `consumer_device_drop_with_live_imports_emits_leak_warning` to
/// assert the warn message fired without coupling to the global
/// subscriber.
mod warn_capture {
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::span::Attributes;
    use tracing::{Event, Level, Metadata, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};

    #[derive(Default)]
    pub struct CapturedWarns(pub Mutex<Vec<String>>);

    impl CapturedWarns {
        pub fn matches(&self, needle: &str) -> bool {
            self.0
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains(needle))
        }
    }

    pub struct WarnCaptureLayer {
        sink: Arc<CapturedWarns>,
    }

    impl WarnCaptureLayer {
        pub fn new(sink: Arc<CapturedWarns>) -> Self {
            Self { sink }
        }
    }

    struct StringVisitor<'a>(&'a mut String);

    impl<'a> Visit for StringVisitor<'a> {
        fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(self.0, "{:?} ", value);
        }
        fn record_str(&mut self, _field: &Field, value: &str) {
            self.0.push_str(value);
            self.0.push(' ');
        }
    }

    impl<S: Subscriber> Layer<S> for WarnCaptureLayer {
        fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
            *metadata.level() <= Level::WARN
        }
        fn on_new_span(&self, _attrs: &Attributes<'_>, _id: &tracing::Id, _ctx: Context<'_, S>) {}
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            if *event.metadata().level() > Level::WARN {
                return;
            }
            let mut captured = String::new();
            event.record(&mut StringVisitor(&mut captured));
            self.sink.0.lock().unwrap().push(captured);
        }
    }
}

#[test]
#[serial]
fn consumer_device_drop_with_live_imports_emits_leak_warning() {
    use streamlib::core::rhi::PixelFormat;
    use streamlib::host_rhi::HostVulkanPixelBuffer;
    use tracing_subscriber::layer::SubscriberExt;

    // Both devices required for the round-trip.
    let host = match try_host() {
        Some(d) => d,
        None => return,
    };
    let consumer = match try_consumer() {
        Some(d) => d,
        None => return,
    };

    // Wire the warn-capture layer for the duration of the test. The
    // subscriber is set as the *thread* default so other concurrent
    // tests don't compete for the global default.
    let captured = Arc::new(warn_capture::CapturedWarns(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::registry().with(warn_capture::WarnCaptureLayer::new(
        Arc::clone(&captured),
    ));
    let _guard = tracing::subscriber::set_default(subscriber);

    // Allocate an exportable HOST_VISIBLE staging buffer on the host
    // and grab its DMA-BUF fd.
    let host_buf = HostVulkanPixelBuffer::new(&host, 64, 64, 4, PixelFormat::Bgra32)
        .expect("host pixel buffer");
    let fd = match host_buf.export_dma_buf_fd() {
        Ok(fd) => fd,
        Err(e) => {
            // Driver doesn't support DMA-BUF export — skip
            // gracefully instead of failing the suite.
            println!("skip: export_dma_buf_fd failed: {e}");
            return;
        }
    };

    // Import on the consumer device. Pick the right memory_type_bits
    // by querying a probe buffer on the consumer's device.
    let probe_info = vk::BufferCreateInfo::builder()
        .size(64 * 64 * 4)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let probe = unsafe { consumer.device().create_buffer(&probe_info, None) }
        .expect("probe buffer");
    let mem_req = unsafe { consumer.device().get_buffer_memory_requirements(probe) };
    unsafe { consumer.device().destroy_buffer(probe, None) };

    let memory = match consumer.import_dma_buf_memory(
        fd,
        mem_req.size,
        mem_req.memory_type_bits,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    ) {
        Ok(m) => m,
        Err(e) => {
            unsafe { libc::close(fd) };
            println!("skip: import_dma_buf_memory failed: {e}");
            return;
        }
    };
    assert_eq!(
        consumer.live_import_allocation_count(),
        1,
        "import bumped the live counter"
    );

    // Forget the memory handle so Drop can't free it from anywhere
    // else, then drop the consumer device. Drop sees
    // `live_allocation_count > 0` and emits the warn.
    let _ = memory; // consume so it's not unused

    drop(consumer);

    // Re-acquire the subscriber guard's recorded warnings; the warn
    // must mention the live count and "leak" (the exact message is
    // "ConsumerVulkanDevice dropping with N live DMA-BUF imports
    // (leak)").
    assert!(
        captured.matches("live DMA-BUF imports") || captured.matches("leak"),
        "expected ConsumerVulkanDevice Drop to emit a leak warning; captured: {:?}",
        captured.0.lock().unwrap()
    );

    // Don't drop host_buf here — the host's allocator would try to
    // free memory the kernel still considers in use by the leaked
    // import. Forget the host buffer so the test exits cleanly; the
    // process tear-down releases all kernel handles. This is
    // acceptable for a test that exercises the *leak* path
    // deliberately.
    std::mem::forget(host_buf);
    std::mem::forget(host);
}
