# Change: one-monotonic-clock

**Change 3 of 3 from the 2026-08-03 align.** Independent of the other two — different
plan section, different ADR, no shared code. Implements the `[one-monotonic-clock]`
DECIDED entry in §Media I/O. ADR: `docs/decisions/one-monotonic-clock.md`.

Scale tier: change artifact, no new ADR (already written by the align). It changes a
contract — what epoch a timestamp carries — without touching the plugin ABI, RHI, wire
format, or processor model.

Recon verified at HEAD `7d334ff7` on 2026-08-03.

## Behavior after this change

Every media timestamp in every language is the machine's monotonic clock — raw
`clock_gettime(CLOCK_MONOTONIC)` on Linux, `mach_absolute_time` on Apple — the same epoch
V4L2 and ALSA driver stamps carry, comparable across every process and node on a host.
`MediaClock` remains the cross-platform naming seam. Each language exports exactly one
name for it.

## Current state — three process-relative epochs, not one

- **`MediaClock`'s Linux arm is the process-relative epoch** the ADR names:
  `core/media_clock.rs:12-17` seeds a `OnceLock<Instant>` on first call and returns
  `start.elapsed()`. The Apple arm (`apple/media_clock.rs:6-31`) is already
  `mach_absolute_time` and machine-wide, so the twins already disagree.
- **A second, independent `MediaClock` type exists** at
  `sdk/streamlib-plugin-sdk/src/media_clock.rs:10-20` — same name, different type, and
  with **no `cfg` arms at all**, so it is process-relative on macOS too, with its own
  `START` distinct from the engine's inside the same process. It dies with the ripout
  (`sdk/streamlib-plugin-sdk`), so this change does not spend budget on it.
- **`SoftwareAudioClock` is a third epoch** (`core/context/audio_clock.rs:190-199`,
  `start_time.elapsed()`), and both platform audio clocks read the right clock and then
  throw the epoch away — `linux/audio_clock.rs:283` (`elapsed_ns = current_ns -
  start_time_ns`) and `apple/audio_clock.rs:157-161`.
- **The wheel already has the duplicate export** the ADR kills: `media_clock_now_ns`
  alongside `monotonic_now_ns` (`python_logging.rs:30` and `:42`), both registered at
  `src/lib.rs:37-41`. `clock.py` re-exports only `monotonic_now_ns`, so it is already the
  correct one-name surface.
- The collision is already flagged in the code:
  `python_processor_link_data_access.rs:133-136` documents its default stamp as "raw
  CLOCK_MONOTONIC, bug-compatible with the old SDK … NOT the MediaClock epoch the
  engine's Rust processors stamp with. Unifying the two epochs is a flagged owner
  decision." This change is that decision landing.
- **Driver stamps are discarded, not rebased** — the V4L2 DMA-BUF path reads only
  `index` and `sequence` and drops `v4l2_buf.timestamp` (`camera_linux.rs:931-949`); the
  cpal audio callback explicitly discards the `_info` carrying `timestamp().capture`
  (`audio_capture_linux.rs:277`). Once the epochs match, using them becomes possible —
  but doing so is A/V-sync work, which is OPEN. Out of scope here.

## Clock scope — RESOLVED by owner, 2026-08-03

**Option (a): the monotonic rule is the data plane's; observability keeps wall clock.**
The line is drawn by what a timestamp is *for*, and it is drawn exhaustively so no future
session has to guess.

### Monotonic — the default, and the only legal clock on the data plane

Everything a processor stamps, reads, or compares. Non-exhaustive by design, because this
is the default: frame and bag timestamps (`iceoryx2/output.rs:449`, the wheel's write path
at `python_processor_link_data_access.rs:137`), audio tick timestamps, `ctx.time` in every
language, decoder pass-through stamps, and anything ever compared against a V4L2 or ALSA
driver stamp. Rust reaches it through `MediaClock`; Python through `monotonic_now_ns`.

### Wall clock — permitted on exactly these four surfaces, and nowhere else

1. Log record `host_ts` (`core/logging/worker.rs:364-369`,
   `compiler_ops/subprocess_escalate.rs:703`).
2. Log record `source_ts` as produced by the language SDKs (`streamlib/log.py:142` and its
   siblings).
3. Log file naming — `started_at_millis` (`core/logging/init.rs:185`,
   `core/logging/paths.rs:22`) and the CLI's rendering of both (`commands/logs.rs:222`).
4. Control-plane pubsub event `timestamp_ns` (`core/pubsub/bus.rs:263-268`).

Their job is correlating StreamLib with the outside world and with other hosts' logs — a
job monotonic time cannot do. Adding a fifth surface is a plan change, not a judgement
call.

### The rule that keeps the two from mixing

A wall-clock value never enters the data plane and is never compared against, subtracted
from, or substituted for a media timestamp. The two are different quantities that happen
to share a unit; a subtraction across them is always a bug.

Rejected: everything monotonic (`streamlib logs` loses human dates), and carrying both
numbers per record (reintroduces exactly the "which one do I compare?" question the ADR
set out to kill).

## REMOVED

Bare patterns — the ship gate greps each line verbatim as a fixed string.

- REMOVED: media_clock_now_ns
  The wheel's duplicate export: `python_logging.rs:23-32`, registration `src/lib.rs:37-40`,
  `python/streamlib/__init__.py:31,58`, stub `_engine.pyi:39,283-289`.

## MODIFIED

- MODIFIED: `core/media_clock.rs:12-17` — the Linux arm becomes raw
  `clock_gettime(CLOCK_MONOTONIC)`, no `OnceLock`, no baseline. This one edit flips the
  epoch for every `MediaClock::now()` caller, including the engine's default frame stamp
  at `iceoryx2/output.rs:449`.
- MODIFIED: `apple/media_clock.rs:24-30` — `host_time_to_nanos` re-queries
  `mach_timebase_info` on every call; cache it while the file is open.
- MODIFIED: `core/context/audio_clock.rs:190-199`, `linux/audio_clock.rs:283`,
  `apple/audio_clock.rs:157-161` — `AudioTickContext.timestamp_ns` stops rebasing to a
  start and carries the machine epoch, so it is comparable with a frame stamp for the
  first time.
- MODIFIED: `core/context/gpu_context.rs:2798-2801` — `escalation_monotonic_ns` is a
  fourth `OnceLock<Instant>`; it routes through the one clock. Its use is internal
  rate-limit deltas, so behavior is unchanged.
- MODIFIED: `core/context/time_context.rs` — `start_ns` and `elapsed_ns()` keep working
  (they subtract an explicit baseline). `elapsed_secs()` stays the "seconds since start"
  API; its unit test asserting `0.09..0.2` (`:69-73`) still holds.
- MODIFIED: `core/logging/event.rs:84` — the `host_ts` doc currently reads "Host monotonic
  receipt timestamp, nanoseconds since UNIX epoch", which is self-contradictory and is
  exactly the confusion the allowlist exists to prevent. It says wall clock, and why.
- MODIFIED: the pin comment at `python_logging.rs:144-145` ("not a process-local origin
  like the media clock's") and the flag comment at
  `python_processor_link_data_access.rs:133-136` — both are falsified by this change.
- ~~MODIFIED: `spikes/streamlib-pyembed-spike/src/monotonic_clock.rs:6-7` carries a stale
  line-referenced claim about `media_clock.rs:12-17` that this change falsifies.~~ —
  Retired 2026-08-07: the spike tree was deleted whole by in-process-hosting-ripout
  (#1714, commit `7ce66f59`); there is no file left to modify.

## ADDED

- ADDED: `cargo xtask check-clock-usage` ~~+ its workflow~~ — the mechanical guard that keeps
  the wall-clock list from growing by accident. It bans wall-clock reads
  (`SystemTime::now`, `chrono::Utc::now`, `time.time_ns`, ~~`Date.now`~~) outside an explicit
  file allowlist holding exactly the four surfaces above, in the shape of the existing
  `lint-logging` / `check-no-escalate-in-lifecycle` checks. Recon found a set of
  `SystemTime` uses that mint *unique names*, not timestamps (`vulkan_graphics_kernel.rs:3119`,
  `surface_share/unix_socket_service.rs:977`, several test helpers); those either move to
  a counter/uuid or join the allowlist with a stated reason — final at implementation.

  > Landed 2026-08-12 (#1728). **No workflow** — #1857 consolidated the per-gate workflows,
  > so the guard is an entry in `ALL_SOURCE_WALKING_GATES` run by the existing `source-gates`
  > job. **No `Date.now` arm** — the Deno SDK is deleted and the engine tree holds no
  > `.ts`/`.js` source, so that scan root does not exist; Rust and Python only.
  >
  > The unique-name uses are **all converted, none allowlisted** — 16 sites across 13 files.
  > The allowlist is per-file, so an entry for `iceoryx2/output.rs` or `thread_runner.rs`
  > would have licensed a wall-clock read in the exact data-plane files the guard exists to
  > protect. `mint_machine_global_unique_name_suffix()` (`core/machine_global_unique_name.rs`)
  > is the one primitive; `streamlib-surface-client`'s test sockets take a `TempDir`, the
  > api-server's name seed takes the OS CSPRNG, and the dead `apple/time.rs::system_time_to_ns`
  > was deleted. The permitted list holds exactly the four surfaces, as five files
  > (`host_ts` has two readers).
- ADDED: an epoch-parity test asserting `MediaClock::now()` and the wheel's
  `monotonic_now_ns` land in the same domain as a directly-read
  `clock_gettime(CLOCK_MONOTONIC)`. The natural home is the existing cross-process gate
  `tests/polyglot_linux_monotonic_clock_parity.rs:112`, which today does **not** include
  `MediaClock` — but its Python/Deno subprocess arms are doomed with the ripout, so the
  surviving shape is host + wheel.

  > Landed as the anticipated host + wheel pair, not one cross-process test: the Rust half is
  > `core/media_clock.rs::now_lands_in_the_kernel_monotonic_domain` (#1725), which brackets
  > `MediaClock::now()` between two direct `clock_gettime(CLOCK_MONOTONIC)` reads, and the
  > Python half is `tests/test_clock_and_log.py::test_monotonic_now_ns_reads_the_kernel_monotonic_clock`.
  > `polyglot_linux_monotonic_clock_parity.rs` went with the ripout.

## Notes (not tickets)

- No test anywhere asserts a frame timestamp is small or near zero, so the epoch flip
  breaks no existing assertion in the engine tree.
- Consumers that format a raw stamp as seconds will print boot-relative values after the
  change — `packages/mp4/.../mp4_writer.rs:839-847,895,1164` and the debug-utilities
  sources that emit near-zero stamps. Deferred re-authoring by doctrine, not this change.
