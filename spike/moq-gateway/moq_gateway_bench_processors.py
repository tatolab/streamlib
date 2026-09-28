# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two Python processors the MoQ gateway spike benchmark runs.

`MoqBenchStampSource` publishes small bags carrying a monotonic stamp at a
stepped rate schedule; `MoqBenchLatencySink` reads them off a remote link and
records, per step, how many arrived, how many the sequence numbering says were
lost, and the arrival latency (monotonic now − stamp, one host, one clock).
`MoqBenchEncodedVideoSink` does the same for encoded video bags, using the
producer's frame stamp.

Configured through the environment the helper processes inherit, because the
benchmark driver sets one schedule for both ends.
"""

import json
import os

from streamlib import (
    RuntimeContextLimitedAccess,
    input,  # noqa: A004 — streamlib's port decorator
    monotonic_now_ns,
    output,
    processor,
)

RATES_PER_SECOND = [
    int(rate) for rate in os.environ.get("MOQ_BENCH_RATES", "100,500,1000,2000,5000").split(",")
]
STEP_NS = int(float(os.environ.get("MOQ_BENCH_STEP_SECONDS", "6")) * 1e9)
START_DELAY_NS = int(float(os.environ.get("MOQ_BENCH_START_DELAY_SECONDS", "10")) * 1e9)
PAD_BYTES = b"\x5a" * int(os.environ.get("MOQ_BENCH_PAD_BYTES", "200"))
RESULTS_PATH = os.environ.get("MOQ_BENCH_RESULTS_PATH", "/tmp/moq-bench-results.json")
# The first part of each step is settling, not measurement.
SETTLE_NS = int(float(os.environ.get("MOQ_BENCH_SETTLE_SECONDS", "1")) * 1e9)
# Most bags one process() call publishes, so a late call cannot burst unboundedly.
MOST_BAGS_PER_CALL = 400


@processor(execution="continuous", interval_ms=1)
class MoqBenchStampSource:
    """Publishes stamped bags at each rate of the schedule in turn."""

    def __init__(self) -> None:
        self.origin_ns = 0
        self.sequence_number = 0
        self.published_in_step: "dict[int, int]" = {}
        self.last_warmup_tick = 0

    @output()
    def stamps(self) -> None: ...

    def setup(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.origin_ns = monotonic_now_ns() + START_DELAY_NS

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        now_ns = monotonic_now_ns()
        if now_ns < self.origin_ns:
            # Warm-up at 10 Hz so the link wires and the path is primed.
            warmup_tick = now_ns // 100_000_000
            if warmup_tick != self.last_warmup_tick:
                self.last_warmup_tick = warmup_tick
                self._publish(ctx, -1, 10)
            return
        step = (now_ns - self.origin_ns) // STEP_NS
        if step >= len(RATES_PER_SECOND):
            return
        rate = RATES_PER_SECOND[step]
        into_step_ns = (now_ns - self.origin_ns) - step * STEP_NS
        due = (into_step_ns * rate) // 1_000_000_000 - self.published_in_step.get(step, 0)
        for _ in range(min(due, MOST_BAGS_PER_CALL)):
            self._publish(ctx, step, rate)
            self.published_in_step[step] = self.published_in_step.get(step, 0) + 1

    def _publish(self, ctx: RuntimeContextLimitedAccess, step: int, rate: int) -> None:
        self.sequence_number += 1
        ctx.outputs.write(
            "stamps",
            {
                "stamp_ns": monotonic_now_ns(),
                "seq": self.sequence_number,
                "step": step,
                "rate": rate,
                "pad": PAD_BYTES,
            },
        )


def _percentile(sorted_values: "list[int]", fraction: float) -> int:
    if not sorted_values:
        return 0
    index = min(len(sorted_values) - 1, int(round(fraction * (len(sorted_values) - 1))))
    return sorted_values[index]


class _PerStepLatencyRecord:
    """Latency and arrival counts, per step, written to the results file."""

    def __init__(self) -> None:
        self.by_step: "dict[int, dict]" = {}
        self.last_written_ns = 0

    def note(self, step: int, rate: int, sequence_number: int, latency_ns: int, measured: bool) -> None:
        record = self.by_step.setdefault(
            step,
            {"rate": rate, "received": 0, "first_seq": sequence_number, "last_seq": sequence_number,
             "latencies_ns": []},
        )
        record["received"] += 1
        record["first_seq"] = min(record["first_seq"], sequence_number)
        record["last_seq"] = max(record["last_seq"], sequence_number)
        if measured:
            record["latencies_ns"].append(latency_ns)

    def write_if_due(self, force: bool = False) -> None:
        now_ns = monotonic_now_ns()
        if not force and now_ns - self.last_written_ns < 1_000_000_000:
            return
        self.last_written_ns = now_ns
        summary = {}
        for step, record in sorted(self.by_step.items()):
            latencies = sorted(record["latencies_ns"])
            expected = record["last_seq"] - record["first_seq"] + 1
            summary[str(step)] = {
                "rate": record["rate"],
                "received": record["received"],
                "expected_between_first_and_last": expected,
                "lost": max(0, expected - record["received"]),
                "measured": len(latencies),
                "p50_ms": _percentile(latencies, 0.50) / 1e6,
                "p99_ms": _percentile(latencies, 0.99) / 1e6,
                "max_ms": (latencies[-1] / 1e6) if latencies else 0.0,
                "mean_ms": (sum(latencies) / len(latencies) / 1e6) if latencies else 0.0,
            }
        temporary = RESULTS_PATH + ".tmp"
        with open(temporary, "w") as results:
            json.dump(summary, results, indent=1)
        os.replace(temporary, RESULTS_PATH)


@processor
class MoqBenchLatencySink:
    """Reads stamped bags and records latency and loss per rate step."""

    def __init__(self) -> None:
        self.record = _PerStepLatencyRecord()
        self.step_first_seen_ns: "dict[int, int]" = {}

    @input(delivery_profile="ordered")
    def stamps(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (bag := ctx.inputs.read("stamps")) is not None:
            now_ns = monotonic_now_ns()
            step = int(bag["step"])
            first_seen = self.step_first_seen_ns.setdefault(step, now_ns)
            self.record.note(
                step, int(bag["rate"]), int(bag["seq"]), now_ns - int(bag["stamp_ns"]),
                measured=now_ns - first_seen >= SETTLE_NS,
            )
        self.record.write_if_due()

    def teardown(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.record.write_if_due(force=True)


@processor
class MoqBenchEncodedVideoSink:
    """Reads encoded video bags and records capture-to-arrival latency."""

    def __init__(self) -> None:
        self.record = _PerStepLatencyRecord()
        self.first_seen_ns = 0
        self.frames = 0

    @input(delivery_profile="ordered")
    def encoded_video(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (read := ctx.inputs.read_with_timestamp("encoded_video"))[0] is not None:
            bag, stamp_ns = read
            now_ns = monotonic_now_ns()
            if self.first_seen_ns == 0:
                self.first_seen_ns = now_ns
            self.frames += 1
            self.record.note(
                0, 0, self.frames, now_ns - int(stamp_ns),
                measured=now_ns - self.first_seen_ns >= SETTLE_NS,
            )
            elapsed_s = (now_ns - self.first_seen_ns) / 1e9
            self.record.by_step[0]["rate"] = round(self.frames / elapsed_s, 2) if elapsed_s > 0 else 0
            self.record.by_step[0]["bitstream_bytes"] = (
                self.record.by_step[0].get("bitstream_bytes", 0) + len(bag.get("bitstream", b""))
            )
        self.record.write_if_due()

    def teardown(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.record.write_if_due(force=True)
