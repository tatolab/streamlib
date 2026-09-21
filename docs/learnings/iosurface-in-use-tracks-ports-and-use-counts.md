# `IOSurfaceIsInUse` tracks ports and use counts, and clears after a reap

## Symptom

- A pool that recycles a slot only when `IOSurfaceIsInUse` is false never
  gets the slot back while a helper process holds the surface — or gets it
  back while the helper is still reading.
- A test that kills a helper holding a surface, reaps it, and immediately
  asserts `!IOSurfaceIsInUse(surface)` passes when run alone and fails when
  run beside other tests.

## Constraint

`IOSurfaceIsInUse` is not "someone holds a reference". Measured on macOS
(Apple Silicon), across processes:

| holder state | in use? |
|---|---|
| a Mach port to the surface exists anywhere (sent, unreceived, or held) | yes |
| a process looked the surface up from its port, then released the port | **no** |
| any process holds `IOSurfaceIncrementUseCount` | yes |
| `IOSurfaceLock` held, no use count | no |

So a helper can cache its `IOSurfaceRef` per pool slot without pinning the
slot, and it claims a frame by raising the use count — a claim the kernel
drops when the process dies. `IOSurfaceGetUseCount` reports only the calling
process's count, so read `IOSurfaceIsInUse` for the cross-process answer.

The release on death is **prompt but asynchronous**. On an idle machine the
count is gone by the time `waitpid` returns; with other work running it
clears 100–400 µs after the reap. The kernel tears the dead task's IOSurface
client down on its own schedule, not as part of the reap.

## Fix pattern

- Every port a process mints or receives is released as soon as it has been
  looked up, or the surface stays in use for as long as the port lives.
  Own ports in a type whose drop deallocates them.
- A recycler treats in-use as "skip this slot for now", never as a one-shot
  test: a slot that reads in use a moment after its holder died simply comes
  back on a later pass.
- A test asserts that the surface clears within a bound after the reap, not
  at it.

## Where this lives

`PixelBufferRingEntry::is_in_use_per_the_kernel` gates the macOS pool;
`surface_share_over_raw_mach.rs` holds the multi-process tests.
