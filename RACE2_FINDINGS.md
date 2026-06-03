# Race #2 investigation findings (transient — delete when fixed)

Companion to ENGINE_GPU_SETUP_RACE_HANDOFF.md. Fix #1 (ungated escalate
wait) is DONE + verified; this doc captures the residual race #2.

## Fix #1 — DONE, verified (banked, uncommitted)

Centralized escalate-scope drain inside the gate:
- `escalate_scope_registry.rs`: added `end_escalate_scope_with` (panic-safe
  exit guard) + `end_escalate_scope_draining` (drains device WHILE the gate
  is held, then releases). `end_escalate_scope` is now `#[cfg(test)]`.
- `runtime_context.rs::with_cdylib_scope` + `limited/escalate.rs::host_gpu_lim_escalate_end`
  now call `end_escalate_scope_draining` instead of release-then-wait.
- `escalate_gate.rs`: added `#[cfg(test)] in_scope()` probe.
- Regression test `drain_runs_while_gate_is_held` + 94 escalate tests pass.
- VERIFIED in published dev.11: all-threads dump shows 0 threads in
  `wait_device_idle` (vs the original ungated Thread 45), 4 correctly parked
  on `EscalateGate::enter`. The ungated-wait race is gone.

Published as `0.4.35-dev.11` to local Gitea (carries fix #1). Runner restored
to pristine dev.10.

## Race #2 — STILL CRASHES, root cause identified

Crash persists in dev.11 (fix #1 present). gdb: crash in the jpeg processor's
GATED `vkCreateComputePipelines`; main thread blocked on an NVIDIA
driver-internal mutex owned by the crash thread.

### Decisive evidence: api_dump (VK_LAYER_LUNARG_api_dump)

api_dump did NOT hide the crash (exit=139). It logs every Vulkan call + thread:
- **Thread 0 (main)**: export pre-warm, then compute kernel pre-warm
  (CreateShaderModule→...→CreateComputePipelines→...→**vkDeviceWaitIdle**→destroy),
  then graphics pre-warm (→CreateGraphicsPipelines→**vkDeviceWaitIdle**→destroy),
  then **vkCreateCommandPool ×2** — the last main-thread GPU work before crash.
- **Thread 1 (jpeg setup)**: **vkDeviceWaitIdle ×2**.
- Crash: main-thread GPU work overlapping the jpeg setup's wait.

### Mechanism (confirmed by the vulkan_device.rs pre-warm comment)

NVIDIA `libnvidia-gpucomp` does a lazy `pthread_once` init on the FIRST
`vkCreatePipelineLayout`. #1203's pre-warm triggers it single-threaded at
device construction. Under info logging the pre-warm (.096-.099) finishes
~50ms before the fan-out setup (.150). Under crash timing (RUST_LOG=warn, no
logging overhead) the main thread's startup GPU work still overlaps the
fan-out's first plugin pipeline-create → concurrent GPU work on the shared
NVIDIA driver → corruption → crash.

This is the handoff's "Compiler pre-warm vs first plugin kernel" candidate.
NOT a shared VkPipelineCache (each create has its own, destroyed per-call).
NOT an app-level external-sync violation (Khronos thread-safety + sync
validation catch NOTHING; validation hides the crash via timing).
`__GL_THREADED_OPTIMIZATIONS=0` does NOT help.

### Open question for the targeted fix

api_dump's `vkCreateCommandPool ×2` on Thread 0 after the pre-warm — need to
pin whether it's device-init tail or a compile/WIRE-phase main-thread GPU op
(a texture ring / link GPU resource) running concurrent with the fan-out.
That's the gate point for the "targeted gate" fix.

## Reproduce race #2 (with fix #1 present)

```bash
# 1. runner Cargo.toml: streamlib = "0.4.35-dev.11"
# 2. build with STREAMLIB_HOME so the cache stays under drone-racer:
cd ~/Repositories/tatolab/drone-racer/racer/runner
source ~/Repositories/tatolab/streamlib/scripts/gitea/registry-token.local.sh
CARGO_TARGET_DIR=/tmp/racer-dev11-target cargo build
export STREAMLIB_HOME=~/Repositories/tatolab/drone-racer/racer
RUST_LOG=warn timeout 12 /tmp/racer-dev11-target/debug/racer-runner; echo exit=$?  # 139
# api_dump (catches the race, names the ops):
VK_INSTANCE_LAYERS=VK_LAYER_LUNARG_api_dump VK_APIDUMP_LOG_FILENAME=/tmp/api_dump.txt \
  VK_APIDUMP_DETAILED=false RUST_LOG=warn timeout 40 /tmp/racer-dev11-target/debug/racer-runner
```

## Final status (this session)

Fix #1 shipped (commit `fix(rhi): drain the device inside the escalate gate`).
Race #2 NOT fixed — established as a deep, **timing-sensitive NVIDIA
driver-internal corruption** during the drone-racer's GPU-heavy startup.

Fixes tried and REVERTED (none moved race #2; don't re-try blindly):
- **#2A — device-wide pipeline-compiler `Mutex`** serializing every
  `vkCreate*Pipelines` + `wait_idle`. dev.12 still cored — in the now-gated
  `wait_idle`; the racer is a raw driver op NOT routed through the RHI.
- **Pre-warm removal** (dev.13): the `#1203` pipeline pre-warm is the main
  thread's biggest GPU work. Removed it (the lock single-threads the gpucomp
  init on the first gated kernel anyway). Still cored — now during the
  **export-pool pre-warm at device init**, with ONLY the main thread touching
  Vulkan (api_dump). So it's not the pipeline pre-warm, and it's not the
  fan-out — #2B (single-threading the fan-out) was ruled out by this.

Ruled out: shared VkPipelineCache; app-level external-sync violation (Khronos
thread-safety + sync validation catch NOTHING); GL threading
(`__GL_THREADED_OPTIMIZATIONS=0` no help); the jpeg plugin creating a second
VkInstance/VkDevice (`nm`/`ldd`/`readelf`: zero Vulkan symbols in the `.so`).

Crash floats with overhead: api_dump (heavy) → device-init export pre-warm;
`RUST_LOG=warn` (light) → fan-out (`JpegDecoder` setup `wait_idle` /
pipeline-create); `RUST_LOG=info` + validation → no crash but exits 1 on the
secondary iceoryx2 issue. So there is **no clean stopgap** — warn cores, info
hits iceoryx2.

**Generic in-tree reproducer is NEGATIVE.**
`libs/streamlib-engine/tests/concurrent_gpu_setup_repro.rs` runs a wide cdylib
fan-out (8 `ComputeKernelTestProcessor` GPU + 4 non-GPU) entirely locally (no
Gitea) and does NOT crash (5/5). So the **cdylib mechanism + generic concurrent
GPU setup are clean** — race #2 needs the drone-racer's *specific workload*
(the real `vulkan-jpeg` fused compute kernel + its texture ring, the real
processors' setup timing, the video link). It is workload-specific, not a
generic streamlib concurrency bug.

Next attempt should either (a) make the reproducer FAITHFUL — load the
pre-built `racer/.streamlib/cache/packages/{jpeg,vadr-vision,mavlink,network}`
`.so`s via `Strategy::Path` against the LOCAL engine + replicate the runner's
pipeline/links, so it reproduces locally with fast gdb iteration (no publish
loop); or (b) treat it as an NVIDIA-driver bug (report upstream / driver
workaround), since every app-level serialization has failed and validation
shows no app-level synchronization violation.

## Iteration loop note

Runner resolves `streamlib` from Gitea by version. Local engine source is
`0.4.35` (publish bumps to `-dev.N`), so a `[patch]` to the local path
fails the version match. The working loop: publish `dev.N` via
`STREAMLIB_PUBLISH_ALL_LIBS=1 CARGO_REGISTRIES_GITEA_TOKEN="Bearer $GITEA_PUBLISH_TOKEN"
scripts/gitea/publish-crates.sh --dev N`, bump the runner to `dev.N`, build
with `CARGO_TARGET_DIR=/tmp/...` + `STREAMLIB_HOME=.../racer` (else the cache
lands outside the drone-racer tree and the package `cargo build` can't find
the `gitea` registry config).
