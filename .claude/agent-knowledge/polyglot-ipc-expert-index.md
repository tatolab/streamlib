# polyglot-ipc-expert — symptom index

Knowledge lives in `docs/`; this file is only routing. Update in the same PR that adds a learning (see `.claude/rules/docs-policy.md`).

Match your symptom, read the doc, then verify its claims against current code — a learning is the best-known state when it was written, not ground truth.

| symptom / trigger | read |
|---|---|
| A test using `PUBSUB.subscribe()` + `PUBSUB.publish()` hangs indefinitely with no error / panic / timeout — PUBSUB silently no-ops without `init()` (subscribe buffers, publish drops) | `docs/learnings/pubsub-lazy-init-silent-noop.md` |
| Startup exit 139 on a multi-processor pipeline and it might be the iceoryx2 WIRE crash (`PublishSubscribeOpenError(DoesNotSupportRequestedMinBufferSize)`, never reaches `setup()`) rather than the GPU race — discriminate via the "Calling setup" grep | `docs/learnings/startup-crash-iceoryx2-wire-vs-gpu-setup-race.md` |
| A subprocess consuming an imported cross-process `VkImage` trips a layout VUID, or gets a discarded/black frame — layout is independent per `VkDevice`; coordinate via QFOT release/acquire or the bridging fallback, not a shared tracker | `docs/learnings/cross-process-vkimage-layout.md` |
| SIGSEGV when a second `VkDevice` is created with the first busy — the evidence behind "subprocess stays consumer-only / one host VkDevice" | `docs/learnings/nvidia-dual-vulkan-device-crash.md` |
