#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Read what an audio processor actually published, off its own output port.

Verifying audio should not need a loopback any more than verifying a frame
needs a display window. An audio block carries its samples inline, so the tap
already exposes everything needed to measure what a processor produced — per
processor, with no second device in the path.

Unlike the loopback fixture beside it, this does need the wheel installed: the
tap's payloads are transport-framed and the engine ships the decoder for them.
That is the right trade here — what is under test is a running engine, so an
engine that will not build has already answered the question.

Two things come back. What can be checked with no signal at all: block cadence,
timestamp continuity, rate, channels, dtype — over the real IPC path rather
than a test harness. And the samples themselves, reassembled into a waveform
that `known_audio_signal.py analyse` measures exactly as it measures a loopback
capture.
"""

import argparse
import json
import sys

import known_audio_signal

# The transport frame every bag arrives inside: a 64-byte port key, then the
# capture timestamp, then the payload length. Read here because the frame's own
# timestamp is a second, independent record of when the block was captured —
# and the two agreeing is the plan's claim that the device stamps the block and
# the engine never re-stamps it.
FRAME_PORT_KEY_BYTES = 64
FRAME_HEADER_BYTES = FRAME_PORT_KEY_BYTES + 8 + 4

# The bag keys an audio block is defined by. Named here rather than imported
# because this reads the wire, and the wire is what the contract is.
SAMPLES_KEY = "samples"
REQUIRED_KEYS = (
    SAMPLES_KEY,
    "sample_rate",
    "channels",
    "sample_count",
    "first_sample_timestamp_ns",
)

# How far a block's start may sit from where the block before it predicted,
# before the stream is not continuous. One device quantum at 48 kHz is ~10.7 ms.
MAX_BLOCK_CONTINUITY_ERROR_MS = 5.0

# The frame header's timestamp and the block's own first-sample timestamp are
# written from one value, so any difference at all is a re-stamp.
MAX_FRAME_VERSUS_BLOCK_TIMESTAMP_ERROR_NS = 0


class TappedBagWasTruncated(Exception):
    """The tap forwards a bounded preview; a larger block arrives cut short.

    Measuring a truncated payload would produce a confident wrong answer, so
    this stops rather than guessing what the missing bytes held.
    """


def frame_timestamp_ns(framed_bag_bytes):
    """When the transport says the block was captured."""
    stamp_at = FRAME_PORT_KEY_BYTES
    return int.from_bytes(
        framed_bag_bytes[stamp_at : stamp_at + 8], "little", signed=True
    )


def audio_blocks_from_tapped_bags(tapped_bags):
    """Decode the tap's payloads into audio blocks, newest ordering ignored.

    Ordering comes from the blocks' own timestamps rather than arrival, so the
    waveform is what the device produced and a gap stays a gap.
    """
    from streamlib._engine import decode_tapped_channel_bag_frame_to_python_object

    blocks = []
    for index, bag in enumerate(tapped_bags):
        if bag.get("hex_truncated"):
            raise TappedBagWasTruncated(
                f"bag {index} is {bag['byte_len']} bytes and the tap forwarded only "
                f"a preview — the block is larger than the tap carries, so what "
                f"arrived cannot be measured"
            )
        framed = bytes.fromhex(bag["hex_preview"])
        decoded = decode_tapped_channel_bag_frame_to_python_object(framed)
        if not isinstance(decoded, dict):
            raise ValueError(f"bag {index} is not a named map: {type(decoded).__name__}")
        missing = [key for key in REQUIRED_KEYS if key not in decoded]
        if missing:
            raise ValueError(f"bag {index} is not an audio block: no {missing[0]!r}")
        decoded["_frame_timestamp_ns"] = frame_timestamp_ns(framed)
        blocks.append(decoded)
    blocks.sort(key=lambda block: block["first_sample_timestamp_ns"])
    return blocks


def continuity_error_ms(blocks):
    """The worst gap or overlap between consecutive blocks, in milliseconds.

    Each block says when its first sample was captured and how many samples it
    carries, so where the next one should start is arithmetic. A block the
    source dropped and did not account for shows up here.
    """
    worst = 0.0
    for earlier, later in zip(blocks, blocks[1:]):
        expected = earlier["first_sample_timestamp_ns"] + (
            earlier["sample_count"] * 1_000_000_000 // earlier["sample_rate"]
        )
        error_ms = (later["first_sample_timestamp_ns"] - expected) / 1_000_000.0
        if abs(error_ms) > abs(worst):
            worst = error_ms
    return round(worst, 3)


def one_format_across(blocks, key):
    """The value every block agrees on, or None where they disagree."""
    values = {block[key] for block in blocks}
    return values.pop() if len(values) == 1 else None


def waveform_from(blocks):
    """The published samples as one signal, in the order they were captured."""
    import numpy

    dtype = one_format_across(blocks, "dtype") or "f32"
    numpy_type = known_audio_signal_numpy_type(dtype)
    channels = one_format_across(blocks, "channels") or 1
    joined = b"".join(bytes(block[SAMPLES_KEY]) for block in blocks)
    interleaved = numpy.frombuffer(joined, dtype=numpy_type)
    if channels > 1:
        interleaved = interleaved.reshape(-1, channels).mean(axis=1)
    if numpy_type == "<i2":
        return interleaved.astype("<f8") / 32768.0
    return interleaved.astype("<f8")


def known_audio_signal_numpy_type(dtype):
    """Little-endian by contract, never the platform's native spelling."""
    for_dtype = {"f32": "<f4", "i16": "<i2"}
    if dtype not in for_dtype:
        raise ValueError(f"dtype {dtype!r} is not one this reads")
    return for_dtype[dtype]


def report_for(blocks):
    """What the processor published, as facts a caller can assert on."""
    sample_rate = one_format_across(blocks, "sample_rate")
    report = {
        "blocks": len(blocks),
        "sample_rate": sample_rate,
        "channels": one_format_across(blocks, "channels"),
        "dtype": one_format_across(blocks, "dtype"),
        "sample_count": one_format_across(blocks, "sample_count"),
        "block_continuity_error_ms": continuity_error_ms(blocks),
        "first_sample_timestamp_ns": blocks[0]["first_sample_timestamp_ns"],
        "frame_versus_block_timestamp_error_ns": max(
            abs(block["_frame_timestamp_ns"] - block["first_sample_timestamp_ns"])
            for block in blocks
        ),
    }
    failures = []
    if report["blocks"] < 2:
        failures.append("blocks")
    for key in ("sample_rate", "channels", "dtype", "sample_count"):
        if report[key] is None:
            # A stream that changed format mid-run is a defect, not a variation.
            failures.append(key)
    if abs(report["block_continuity_error_ms"]) > MAX_BLOCK_CONTINUITY_ERROR_MS:
        failures.append("block_continuity_error_ms")
    if (
        report["frame_versus_block_timestamp_error_ns"]
        > MAX_FRAME_VERSUS_BLOCK_TIMESTAMP_ERROR_NS
    ):
        # The two are written from one value. A difference means something
        # re-stamped the frame at publication rather than carrying the
        # device's own instant of capture.
        failures.append("frame_versus_block_timestamp_error_ns")
    report["failed"] = failures
    report["verdict"] = "PASS" if not failures else "FAIL"
    return report


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tapped_bags_json", help="what `streamlib tap` returned")
    parser.add_argument("--waveform", help="write the published samples here as WAV")
    arguments = parser.parse_args(argv[1:])

    with open(arguments.tapped_bags_json) as source:
        tapped = json.load(source)
    bags = tapped["bags"] if isinstance(tapped, dict) else tapped
    if not bags:
        print(
            json.dumps({"verdict": "FAIL", "reason": "the tap returned no bags"}, indent=2)
        )
        return 1

    blocks = audio_blocks_from_tapped_bags(bags)
    report = report_for(blocks)
    if arguments.waveform:
        sample_rate = report["sample_rate"] or known_audio_signal.SAMPLE_RATE
        known_audio_signal.write_wav(
            arguments.waveform, waveform_from(blocks), sample_rate
        )
        report["waveform"] = arguments.waveform
    print(json.dumps(report, indent=2))
    return 0 if report["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
