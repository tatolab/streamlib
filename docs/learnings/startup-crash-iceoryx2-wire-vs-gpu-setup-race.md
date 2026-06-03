# Two different startup SIGSEGVs both exit 139 — don't conflate the iceoryx2 WIRE crash with the GPU concurrent-setup race

## Symptom

A multi-processor pipeline (the drone-racer host: `UdpSource → VadrVisionDepayloader
→ JpegDecoder → RacerPilot` + a MAVLink path) on NVIDIA Linux exits **139**
(SIGSEGV) during startup under `RUST_LOG=warn`. There are **two distinct
crashes that both present as exit 139**, at two different phases, and they are
easy to conflate:

1. **iceoryx2 WIRE crash.** A `Runtime("Failed to open/create service:
   PublishSubscribeOpenError(DoesNotSupportRequestedMinBufferSize)")` during the
   compiler's WIRE phase, followed by a SIGSEGV in the failed-start teardown.
   The run **never reaches processor `setup()`**.
2. **GPU concurrent-setup race.** A SIGSEGV deep in `libnvidia-glcore` via
   `vkCreateComputePipelines` *during* `setup()`, from the main thread and a
   setup-fan-out thread being concurrently inside the NVIDIA driver, contending
   a driver-internal mutex.

## The trap

Both exit 139, so a bare exit code tells you nothing. The decisive
discriminator is **whether the run reached setup**:

```bash
grep -q "Calling setup" "$run.log" && echo "reached setup → GPU race candidate" \
                                    || echo "no setup → iceoryx2 WIRE crash"
```

Empirically (this host, NVIDIA 595.71.05 / RTX 3090), under `RUST_LOG=warn` the
**iceoryx2 WIRE crash is the one that actually fires** — every normal-run crash
checked was `no-setup` with the `DoesNotSupportRequestedMinBufferSize` line. The
GPU race only appeared **under gdb**, because gdb's overhead shifts the wiring
timing so the WIRE failure is dodged and the run slips through to setup. So a
gdb backtrace showing the GPU crash is **not** evidence that the GPU race is
what crashes in production — it's evidence gdb's timing got you past the real
(WIRE) failure.

## What was actually wrong

The drone-racer's normal-run crash was the **iceoryx2 WIRE issue**: a
destination with multiple input ports shares one per-destination iceoryx2
service, but each inbound link sized that shared service's subscriber buffer
from only its own output schema's `max_queued_messages`; a later, deeper link's
`open_or_create` then failed. Sizing the shared service to the **max** depth
across the destination's inbound links (plus pinning `max_subscribers = 1`)
fixes it. After that fix the runner reaches setup and runs clean — **35/35
cold runs, zero GPU-race manifestations.**

The GPU concurrent-setup race is **real but latent**: gdb-provable, but 0/35 in
normal runs. Its candidate fix is the industry-standard one (funnel all
`vkCreate*Pipelines` through a single dedicated compile thread — NVIDIA's and
every surveyed engine's documented practice), deferred until a reliable,
non-gdb reproducer exists. An ordering "join the setup fan-out" barrier was
prototyped and rejected: it changes `start()` semantics, carries a
subprocess-host deadlock edge, and could not be validated against a crash that
doesn't fire.

## The lesson

- **A startup 139 is ambiguous on NVIDIA — classify it by phase** (`reached
  setup?`) before attributing it to GPU concurrency. An IPC/wiring failure and a
  GPU-driver race look identical at the exit-code level.
- **gdb (and api_dump, and validation layers) change startup timing enough to
  move *which* crash you hit.** Use them to read a backtrace, never to decide
  *which* failure is the production one. Confirm the production failure mode
  under the un-instrumented timing that actually reproduces it.
- **Re-verify the symptom after each fix.** Once the iceoryx2 fix made the
  runner reach setup, a 5-minute `30×` re-run would have shown the GPU race was
  latent — much cheaper than assuming the original crash framing still held.

## Reference

- iceoryx2 sizing fix: `core/compiler/compiler_ops/open_iceoryx2_service_op.rs`
  (`max_queued_messages_for_dest`) + `iceoryx2/node.rs` (`max_subscribers`).
- The latent GPU race's mechanism (main-vs-fan-out glcore contention) and the
  funnel candidate fix live in the tracked issue for it, not here — this file
  is the *diagnostic* learning, not the fix proposal.
- Sibling: [`concurrent-vkdevicewaitidle-threading.md`](concurrent-vkdevicewaitidle-threading.md)
  (the already-fixed sibling race; the validation layer DOES name that one,
  which is part of how we know the GPU concurrent-setup race is a different,
  validation-silent beast).
