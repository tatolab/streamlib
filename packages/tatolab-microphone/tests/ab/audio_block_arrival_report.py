# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Score the built-in and module arms' arrival recordings against the ticket's bar.

Each recording is the CSV `AudioBlockArrivalRecorder` writes. Per arm:

- dropped blocks: gaps in the device timestamps wider than half a block, in
  blocks, plus every message the link reported lost;
- late blocks: blocks whose capture-to-arrival latency exceeds the arm's own
  median by more than one block duration — a stall, not a constant offset;
- block-arrival jitter: |inter-arrival interval − block duration|, p50/p99/max.

The module passes when it drops and delivers late no more blocks than the
built-in, and its p99 jitter is within 1 ms of the built-in's.

Usage: audio_block_arrival_report.py <built_in.csv> <module.csv> [--warmup-seconds N]
       [--markdown out.md] [--json out.json]
"""

from __future__ import annotations

import argparse
import csv
import json
import statistics
import sys
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path

JITTER_P99_ALLOWANCE_MS = 1.0
NS_PER_MS = 1_000_000


@dataclass(frozen=True)
class RecordedArrival:
    arrival_ns: int
    first_sample_timestamp_ns: int
    sample_count: int
    sample_rate: int
    lost_since_last_read: int


@dataclass(frozen=True)
class ArmScore:
    blocks: int
    seconds_measured: float
    block_frames: int
    sample_rate: int
    block_duration_ms: float
    dropped_blocks_from_timestamp_gaps: int
    blocks_lost_on_the_link: int
    dropped_blocks: int
    late_blocks: int
    latency_ms_p50: float
    latency_ms_p99: float
    latency_ms_max: float
    jitter_ms_p50: float
    jitter_ms_p99: float
    jitter_ms_max: float


def read_recording(path: Path) -> list[RecordedArrival]:
    with path.open(encoding="ascii") as recording:
        return [
            RecordedArrival(*(int(row[column]) for column in RecordedArrival.__dataclass_fields__))
            for row in csv.DictReader(recording)
        ]


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return float("nan")
    index = min(len(ordered) - 1, max(0, round(fraction * (len(ordered) - 1))))
    return ordered[index]


def score_arm(arrivals: list[RecordedArrival], warmup_seconds: float) -> ArmScore:
    if len(arrivals) < 3:
        raise SystemExit(f"only {len(arrivals)} blocks were recorded; nothing to score")
    warmup_end_ns = arrivals[0].arrival_ns + int(warmup_seconds * 1e9)
    measured = [arrival for arrival in arrivals if arrival.arrival_ns >= warmup_end_ns]
    if len(measured) < 3:
        raise SystemExit("the warm-up swallowed the whole recording")

    block_frames, _ = Counter(arrival.sample_count for arrival in measured).most_common(1)[0]
    sample_rate, _ = Counter(arrival.sample_rate for arrival in measured).most_common(1)[0]
    block_duration_ns = block_frames * 1e9 / sample_rate

    dropped_from_gaps = 0
    for previous, current in zip(measured, measured[1:]):
        expected_ns = previous.first_sample_timestamp_ns + (
            previous.sample_count * 1e9 / previous.sample_rate
        )
        gap_ns = current.first_sample_timestamp_ns - expected_ns
        if gap_ns > block_duration_ns / 2:
            dropped_from_gaps += max(1, round(gap_ns / block_duration_ns))
    lost_on_the_link = sum(arrival.lost_since_last_read for arrival in measured[1:])

    latencies_ms = [
        (
            arrival.arrival_ns
            - arrival.first_sample_timestamp_ns
            - arrival.sample_count * 1e9 / arrival.sample_rate
        )
        / NS_PER_MS
        for arrival in measured
    ]
    median_latency_ms = statistics.median(latencies_ms)
    block_duration_ms = block_duration_ns / NS_PER_MS
    late_blocks = sum(
        1 for latency_ms in latencies_ms if latency_ms - median_latency_ms > block_duration_ms
    )

    jitters_ms = [
        abs((current.arrival_ns - previous.arrival_ns) - block_duration_ns) / NS_PER_MS
        for previous, current in zip(measured, measured[1:])
    ]

    return ArmScore(
        blocks=len(measured),
        seconds_measured=(measured[-1].arrival_ns - measured[0].arrival_ns) / 1e9,
        block_frames=block_frames,
        sample_rate=sample_rate,
        block_duration_ms=block_duration_ms,
        dropped_blocks_from_timestamp_gaps=dropped_from_gaps,
        blocks_lost_on_the_link=lost_on_the_link,
        dropped_blocks=dropped_from_gaps + lost_on_the_link,
        late_blocks=late_blocks,
        latency_ms_p50=median_latency_ms,
        latency_ms_p99=percentile(latencies_ms, 0.99),
        latency_ms_max=max(latencies_ms),
        jitter_ms_p50=percentile(jitters_ms, 0.50),
        jitter_ms_p99=percentile(jitters_ms, 0.99),
        jitter_ms_max=max(jitters_ms),
    )


def verdict(built_in: ArmScore, module: ArmScore) -> dict[str, bool]:
    return {
        "dropped_blocks_no_more_than_the_built_in": module.dropped_blocks <= built_in.dropped_blocks,
        "late_blocks_no_more_than_the_built_in": module.late_blocks <= built_in.late_blocks,
        "jitter_p99_within_1ms_of_the_built_in": module.jitter_ms_p99
        <= built_in.jitter_ms_p99 + JITTER_P99_ALLOWANCE_MS,
    }


def markdown_table(built_in: ArmScore, module: ArmScore) -> str:
    rows = [
        ("blocks measured", "blocks", "{:d}"),
        ("seconds measured", "seconds_measured", "{:.1f}"),
        ("block", "block_frames", "{:d} frames"),
        ("sample rate", "sample_rate", "{:d} Hz"),
        ("dropped blocks (timestamp gaps)", "dropped_blocks_from_timestamp_gaps", "{:d}"),
        ("blocks lost on the link", "blocks_lost_on_the_link", "{:d}"),
        ("late blocks (> median + 1 block)", "late_blocks", "{:d}"),
        ("capture→arrival latency p50", "latency_ms_p50", "{:.2f} ms"),
        ("capture→arrival latency p99", "latency_ms_p99", "{:.2f} ms"),
        ("capture→arrival latency max", "latency_ms_max", "{:.2f} ms"),
        ("arrival jitter p50", "jitter_ms_p50", "{:.3f} ms"),
        ("arrival jitter p99", "jitter_ms_p99", "{:.3f} ms"),
        ("arrival jitter max", "jitter_ms_max", "{:.3f} ms"),
    ]
    lines = [
        "| | built-in `MicrophoneSource` | `tatolab.microphone` `MicrophoneSource` |",
        "|---|---:|---:|",
    ]
    for label, field, spelling in rows:
        lines.append(
            f"| {label} | {spelling.format(getattr(built_in, field))} "
            f"| {spelling.format(getattr(module, field))} |"
        )
    return "\n".join(lines)


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Score the microphone A/B recordings.")
    parser.add_argument("built_in_recording", type=Path)
    parser.add_argument("module_recording", type=Path)
    parser.add_argument("--warmup-seconds", type=float, default=2.0)
    parser.add_argument("--markdown", type=Path)
    parser.add_argument("--json", type=Path)
    parsed = parser.parse_args(arguments)

    built_in = score_arm(read_recording(parsed.built_in_recording), parsed.warmup_seconds)
    module = score_arm(read_recording(parsed.module_recording), parsed.warmup_seconds)
    checks = verdict(built_in, module)
    passed = all(checks.values())

    table = markdown_table(built_in, module)
    summary = "\n".join(f"- {name}: {'yes' if held else 'NO'}" for name, held in checks.items())
    report = f"{table}\n\n{summary}\n\nRESULT: {'PASS' if passed else 'MISS'}\n"
    print(report)
    if parsed.markdown:
        parsed.markdown.write_text(report, encoding="utf-8")
    if parsed.json:
        parsed.json.write_text(
            json.dumps(
                {"built_in": asdict(built_in), "module": asdict(module), "checks": checks},
                indent=2,
            ),
            encoding="utf-8",
        )
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
