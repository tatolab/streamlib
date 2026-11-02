# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The A/B scorer on recordings whose drops, stalls and jitter are known."""

import importlib.util
import sys
from pathlib import Path
from types import ModuleType

REPORT_PATH = Path(__file__).parent / "ab" / "audio_block_arrival_report.py"
BLOCK_FRAMES = 1024
SAMPLE_RATE = 48_000
BLOCK_NS = BLOCK_FRAMES * 1_000_000_000 // SAMPLE_RATE


def _load_report_module() -> ModuleType:
    spec = importlib.util.spec_from_file_location("audio_block_arrival_report", REPORT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


report = _load_report_module()


def _steady_recording(blocks: int, latency_ns: int = 5_000_000) -> list:
    return [
        report.RecordedArrival(
            arrival_ns=index * BLOCK_NS + BLOCK_NS + latency_ns,
            first_sample_timestamp_ns=index * BLOCK_NS,
            sample_count=BLOCK_FRAMES,
            sample_rate=SAMPLE_RATE,
            lost_since_last_read=0,
        )
        for index in range(blocks)
    ]


def test_a_steady_recording_has_no_drops_no_late_blocks_and_no_jitter() -> None:
    score = report.score_arm(_steady_recording(500), warmup_seconds=0)

    assert score.dropped_blocks == 0
    assert score.late_blocks == 0
    assert score.jitter_ms_p99 < 0.001
    assert abs(score.latency_ms_p50 - 5.0) < 0.001


def test_missing_timestamps_count_as_dropped_blocks() -> None:
    recording = _steady_recording(500)
    del recording[100:103]

    assert report.score_arm(recording, warmup_seconds=0).dropped_blocks_from_timestamp_gaps == 3


def test_link_losses_count_as_dropped_blocks() -> None:
    recording = _steady_recording(500)
    recording[200] = report.RecordedArrival(
        **{**recording[200].__dict__, "lost_since_last_read": 2}
    )

    assert report.score_arm(recording, warmup_seconds=0).dropped_blocks == 2


def test_a_stall_makes_the_blocks_behind_it_late() -> None:
    recording = _steady_recording(500)
    stall_ns = 3 * BLOCK_NS
    for index in (300, 301):
        recording[index] = report.RecordedArrival(
            **{**recording[index].__dict__, "arrival_ns": recording[index].arrival_ns + stall_ns}
        )

    score = report.score_arm(recording, warmup_seconds=0)
    assert score.late_blocks == 2
    assert score.dropped_blocks == 0


def test_the_verdict_allows_one_millisecond_of_extra_p99_jitter_and_no_more() -> None:
    built_in = report.score_arm(_steady_recording(500), warmup_seconds=0)
    within = built_in.__class__(**{**built_in.__dict__, "jitter_ms_p99": built_in.jitter_ms_p99 + 0.9})
    beyond = built_in.__class__(**{**built_in.__dict__, "jitter_ms_p99": built_in.jitter_ms_p99 + 1.1})

    assert all(report.verdict(built_in, within).values())
    assert not report.verdict(built_in, beyond)["jitter_p99_within_1ms_of_the_built_in"]
