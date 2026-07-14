# Concurrent `vkDeviceWaitIdle` crashes the NVIDIA driver — hold every queue mutex

## Symptom

An NVIDIA Linux process with **concurrent multi-processor GPU setup** (several
processors each touching the GPU on their own setup thread) SIGSEGVs deep inside
`libnvidia-glcore.so` — typically through `vkCreateComputePipelines` /
`vkCreateGraphicsPipelines`:

```
Thread N received signal SIGSEGV.
0x… in ?? () from /lib/x86_64-linux-gnu/libnvidia-glcore.so.<ver>
  … vulkanalia …::create_compute_pipelines
  … create_compute_pipeline_with_cache
  … VulkanComputeKernel::new
```

The crash is **deterministic under that workload** but reproduces in **no
isolated test**:

- Single-threaded kernel creation (`gpu_decode`, `simple_decoder`) — clean.
- Many compute kernels created concurrently in one process **with no
  intervening drops/submits** — clean.
- Only the full multi-processor runtime (e.g. a plugin host wiring up 5+
  processors, some creating pipelines, some submitting, some tearing down
  GPU resources, all during the concurrent `setup()` fan-out) crashes.

Because the crash is in the driver and the proximate frame is an innocent
pipeline-create, it masquerades as a pipeline / shader-compiler problem, or
gets blamed on whatever happened most recently (a CUDA probe, a version skew,
a pre-warm) — all red herrings.

## The diagnostic that cracks it: the validation layer

Enable the Khronos validation layer (`STREAMLIB_VULKAN_VALIDATION=1`) and the
real cause prints right before the crash:

```
Validation Error: [ UNASSIGNED-Threading-Info ]
vkDeviceWaitIdle(): Couldn't find VkQueue Object 0x… —
  may indicate a bug in the application.
```

**Lesson: when an NVIDIA GPU crash has no obvious cause and only reproduces
under concurrency, turn on the validation layer first.** A driver SIGSEGV with
a clean call stack is almost always an external-synchronization (threading)
violation the driver tolerated until it corrupted its own state. The layer
names it in one line; without it you can burn a day eliminating innocent
suspects.

## Root cause

Per the Vulkan spec, `vkDeviceWaitIdle` is **externally synchronized over the
`VkDevice` and every `VkQueue` it owns** — it is equivalent to calling
`vkQueueWaitIdle` on all of them. Calling it on one thread while **any** other
thread runs a queue operation (`vkQueueSubmit2`, `vkQueuePresentKHR`) on **any**
of those queues is undefined behavior. NVIDIA's driver doesn't validate it — it
corrupts internal queue/device bookkeeping and the next driver entry point
(often pipeline creation on a third thread) dereferences the garbage and
crashes.

In a plugin/processor engine this is easy to hit by accident: each processor's
`setup()` runs on its own thread concurrently, and GPU resource **`Drop`
impls** (compute/graphics/RT kernels, acceleration structures, codec sessions,
present targets) routinely call `device_wait_idle()` to drain the GPU before
destroying handles. One processor's teardown wait races another processor's
submit.

## Fix

`vkDeviceWaitIdle` must acquire **every per-queue mutex** (plus the device
mutex) before it runs, so no queue op can be in flight. Provide a single
guarded helper and route **every** consumer through it — never call the raw
`device_wait_idle()` outside that helper:

```rust
// HostVulkanDevice::wait_idle — the ONLY place raw device_wait_idle is allowed.
pub fn wait_idle(&self) -> Result<()> {
    // Fixed lock order vs. the single-queue-mutex submit/present paths, so no
    // deadlock: graphics → transfer → compute → video-encode → video-decode → device.
    let _g  = self.graphics_queue_mutex.lock()…;
    let _t  = self.transfer_queue_mutex.lock()…;
    let _c  = self.compute_queue_mutex.lock()…;
    let _ve = self.video_encode_queue_mutex.lock()…;
    let _vd = self.video_decode_queue_mutex.lock()…;
    let _dev = self.device_mutex.lock()…;
    unsafe { self.device.device_wait_idle() }…
}
```

The lock order matters: `submit_to_queue` / `present_to_queue` each take exactly
**one** queue mutex, so `wait_idle` taking all of them in a fixed order can
never deadlock against them.

**Every `Drop` impl and teardown path that drains the GPU calls
`<host_device>.wait_idle()`, not the raw `vulkanalia` `device_wait_idle()`.**
Structs that previously held only a raw `vulkanalia::Device` for their `Drop`
already tend to hold an `Arc<HostVulkanDevice>` too (for allocation/queue
access) — route the wait through that.

## Why it stays fixed

A grep-style CI lint (`cargo run -p xtask -- check-device-wait-idle`) bans raw
`.device_wait_idle(` anywhere in the engine except the helper's own file. The
doc rule ("every consumer routes through `wait_idle`") is only as good as the
next contributor's memory; the lint makes a raw call fail CI. Test files and
inline `#[cfg(test)]` modules are exempt; a `streamlib:allow-raw-device-wait-idle`
line pragma is the reviewed escape hatch.

## Why isolated tests miss it

The race needs a **mix** of concurrent queue submits and device waits across
threads sharing one `VkDevice`. A create-only concurrency test never submits or
drops mid-flight, so there's no queue op to race the wait. You either reproduce
it in the real multi-processor runtime or construct a test that explicitly
interleaves submit + drop (kernel `Drop` → wait) across a thread barrier. The
cheap, reliable regression lock is the lint, not a timing-dependent stress test.

## Reference

- Helper + the fixed lock order: `HostVulkanDevice::wait_idle` in
  `runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs`.
- CI lint: `xtask/src/check_device_wait_idle.rs` (`cargo run -p xtask --
  check-device-wait-idle`).
- The per-queue submit mutexes it coordinates with: `submit_to_queue` /
  `present_to_queue` / `mutex_for_queue` in the same file.
- Spec: [Vulkan synchronization — `vkDeviceWaitIdle` is externally
  synchronized over the device and all its queues](https://docs.vulkan.org/spec/latest/chapters/cmdbuffers.html#vkDeviceWaitIdle).
