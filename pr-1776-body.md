## Summary

A Python processor can run several compute passes as one batch:

```python
with ctx.gpu_full_access.kernel_dispatch_batch() as batch:
    batch.dispatch(self.blur_horizontal, bindings={...}, group_count=(...))
    batch.dispatch(self.blur_vertical, bindings={...}, group_count=(...))
```

Two passes cost one round trip, one submission and one fence wait instead of two
of each. Nothing about the synchronous contract changes: the scope returns when
the GPU work has retired and the writes are visible, and no fence or timeline
value reaches Python.

**The batch is one escalate op, not a scope held open across the wire.**
`batch.dispatch()` validates and accumulates helper-side; leaving the scope sends
one `run_compute_kernel_batch`. Holding the scope open would hold the engine's
escalate gate — which serializes runtime-wide and waits for device idle — across
arbitrary user Python between the dispatch calls, making one processor stall
every other to save that processor's submissions. That is the axis the placement
model exists to protect, so the accumulate-then-send shape is not an
optimisation here.

**Barriers are per-binding image barriers.** Each dispatch's bindings are
barriered into the layout its descriptor requires (`GENERAL` for a storage
image, `SHADER_READ_ONLY_OPTIMAL` for a sampled texture), from the layout the
recording has that image in so far. The same barrier carries the write-then-read
edge — `COMPUTE_SHADER`/`SHADER_WRITE` as source — so pass N+1 observes pass N's
stores. A bare global memory barrier would not have been enough: a real two-pass
blur samples what the previous pass storage-wrote, which needs the transition
too.

**One kernel per batch, refused by name.** `VulkanComputeKernel`'s descriptor
pool is `max_sets(1)`, so recording two dispatches against one kernel gives both
the last `set_*` call's bindings — silently, since nothing has executed yet.
Per-dispatch descriptor sets is exactly what the plan's Rust bindings-at-dispatch
convergence delivers, and `python-kernel-surface.md` sequences that after this
change, so this refuses rather than pulling it forward. Both the wheel (caller's
own stack) and the engine (so the wheel is not the only guard) refuse it.

**Counting.** `HostVulkanDevice` gains two counters beside the existing
`live_allocation_count`: every `vkQueueSubmit2`, and the fence waits the two
compute-dispatch paths take (the command recorder and `VulkanComputeKernel`,
routed through one counted helper). They are what makes the batching claim
falsifiable — elapsed time cannot stand in for it.

## Closes

Closes #1776

## Exit criteria

- [x] `KernelDispatchBatch` + `ctx.gpu_full_access.kernel_dispatch_batch()`, over the existing `RhiCommandRecorder`: one `begin()` → N `record_dispatch` → one submit → one fence wait
- [x] `batch.dispatch(kernel, bindings=…, group_count=…)` — receiver explicit, mirroring `recorder.record_dispatch(kernel, …)`
- [x] A barrier between consecutive dispatches so pass N+1 sees pass N's writes
- [x] Dispatch stays synchronous; no fence or timeline vocabulary reaches Python
- [x] Bindings passed per dispatch, by name, never persisting on the kernel
- [x] The scope does not strand the recorder on the exception path (`abort_recording` on a mid-record failure; a raise in the block sends nothing at all)
- [x] `tracing` only, no `todo!()`, `_engine.pyi` entry present and `mypy.stubtest`-clean
- [x] The demo: a two-pass filter costing one submission and one fence wait

## Test plan

**Engine** (`cargo test -p streamlib-engine --lib compute_kernel_dispatch` — 25 pass on the rig, 5 new):

- `a_later_pass_in_a_batch_reads_what_an_earlier_pass_wrote` — three textures, A→B→C, two shaders that do not commute (`+40/255` then `×2`), so a swapped order or a pass reading the seed instead of the intermediate lands on different pixels. Also asserts the published layouts, which the pixels cannot check: an `UNDEFINED` source layout licenses a discard this driver declines to take, so barriering every pass from the pre-batch layout reads back correctly while being wrong by the spec. **Falsified** — removing the in-recording layout advance fails this assertion (`VulkanLayout(0)` vs `(5)`).
- `a_batch_costs_one_submission_and_one_stall_where_separate_dispatches_cost_n` — both arms in one test: two dispatches batched, then the same two as separate `run_compute_kernel` ops. Asserts the separate arm submits twice, without which the batched `== 1` would pass on a dead counter.
- `a_batch_naming_one_kernel_twice_is_refused_saying_why`
- `a_refused_batch_submits_nothing_and_leaves_the_recorder_usable` — fails on dispatch 2, so dispatch 1 was already planned; asserts the submission count is unmoved, the first pass's output pixels are untouched, and a later batch still runs (which `begin()` would refuse if a recording had been left open).
- `an_empty_batch_submits_nothing_and_is_not_an_error`
- `escalate_request_vectors_round_trip` — golden wire vector for the new op.

**Python** (`XDG_RUNTIME_DIR=… .venv/bin/python -m pytest tests/test_kernel_dispatch_batch.py` — 7 pass on the rig; `requires_gpu`, so rig-only): the scope as the change file spells it, a second scope after the first returns, a raise propagating unsuppressed with a fresh batch running after it, and the three refusals (unknown binding at the dispatch line, one kernel twice, a spent batch).

`tests/test_compute_kernel.py` (13) still passes. `mypy.stubtest streamlib._engine` clean. `cargo xtask check-boundaries` clean.

Pixel proof stays engine-side: a helper child holds no mapping for an acquired texture, so the Python tests prove the surface and the Rust tests prove the pixels.

## Notes for owner

1. **The macOS cross-compile could not run here.** `cargo check -p streamlib-python-wheel --target aarch64-apple-darwin` fails in `iceoryx2-pal-posix`'s build script (`'libproc.h' file not found`) — no macOS SDK on this box, pre-existing and unrelated to this change. The non-Linux arms were reviewed by hand instead; one pre-existing `expect(dead_code)` on `PythonGpuContextFullAccess::helper_process_exchange_client` had to go, because `kernel_dispatch_batch()` reads that field on every platform.

2. **`PROTOCOL_VERSION` was not bumped.** It is already 2, and a new op reshapes no existing one — a stale helper simply never sends it. Flagging because `python-kernel-surface.md`'s MODIFIED list ties the bump to "the escalate ops change shape".

3. **`python-kernel-surface.md` still anchors on `escalate_request.yaml`** (`:277`, `:283-289`, `:352-377`, `:610-628`, `:952-978`, `:1015-1038`). #1773 replaced that file with hand-written wire types in `subprocess_escalate_wire_types/escalate_request.rs`. Left alone rather than swept — it is the live change artifact and re-anchoring it wholesale is bigger than this ticket — but it will mislead the next reader.

4. **The single-dispatch path records no layout transition.** `bind_and_dispatch_compute_kernel` binds and dispatches without barriering, so two passes run as two separate `run_compute_kernel` ops get neither the transition nor the write-then-read edge that the same two passes get inside a batch. Pre-existing (it predates this ticket), and the batch does not make it worse — but it means "two dispatches" and "a batch of two" are not merely different in cost. Worth a decision on whether the single path should barrier too; I did not touch it, since the ticket's scope is the batch.

5. **Two textually identical shaders are one kernel.** `create_compute_kernel` caches by source key, so a separable blur written as one shader with a direction push constant cannot be two kernel objects, and therefore cannot be one batch. It has to be two batches, or two textually distinct shaders (which is what the change file's own example uses). This retires with the bindings-at-dispatch convergence and its per-dispatch descriptor sets.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
