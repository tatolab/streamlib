#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Read what an audio processor actually published, off its own output port.

Verifying audio should not need a loopback any more than verifying a frame
needs a display window. An audio block carries its samples inline, so the tap
already exposes everything needed to measure what a processor produced — per
processor, with no second device in the path.

Unlike the loopback fixture beside it, this needs the wheel installed: the tap's
payloads are transport-framed and the engine ships the decoder for them, and
`streamlib.AudioBlock` is the cast that says what a well-formed block is. That
is the right trade — what is under test is a running engine, so an engine that
will not build has already answered the question.

Two things come back. What can be checked with no signal at all: block cadence,
timestamp continuity, rate, channels, dtype — over the real IPC path rather
than a test harness. And the samples themselves, placed by their own timestamps
into a waveform that `known_audio_signal.py analyse` measures exactly as it
measures a loopback capture.
"""

import argparse
import json
import sys
from typing import NamedTuple

import known_audio_signal

# The transport frame every bag arrives inside: a 64-byte port key, then the
# timestamp, then the payload length. Read here so the frame's stamp can be
# compared with the block's own. Both are written by the producer, so what the
# comparison proves is that nothing re-stamped the frame at publication — NOT
# that the stamp is the device's, which needs evidence this tool cannot see.
FRAME_PORT_KEY_BYTES = 64

# How far a block may sit from where the one before it predicted, as a fraction
# of a block's own duration. A fraction rather than a constant because the
# quantum is the device's to choose: a bound written for 512 samples at 48 kHz
# stops discriminating entirely at 128.
MAX_CONTINUITY_ERROR_AS_FRACTION_OF_A_BLOCK = 0.5

# Loss spread thinly stays under the per-gap bound while still adding up. Scaled
# by the number of gaps, so a longer tap is not a laxer one: a fixed budget
# makes sensitivity a function of how many bags were asked for.
MAX_CUMULATIVE_CONTINUITY_ERROR_PER_GAP_AS_BLOCKS = 0.15


class TappedBagWasTruncated(Exception):
    """The tap forwards a bounded preview; a larger block arrives cut short.

    Measuring a truncated payload would produce a confident wrong answer, so
    this stops rather than guessing what the missing bytes held.
    """


class TappedAudioBlock(NamedTuple):
    """One published block, with what the transport said about it."""

    block: object
    frame_timestamp_ns: int


def frame_timestamp_ns(framed_bag_bytes):
    """When the transport says the block was captured."""
    stamp_at = FRAME_PORT_KEY_BYTES
    return int.from_bytes(
        framed_bag_bytes[stamp_at : stamp_at + 8], "little", signed=True
    )


def audio_blocks_from_tapped_bags(tapped_bags, preview_bound_bytes=None):
    """Decode the tap's payloads into audio blocks, arrival order ignored.

    Ordering comes from the blocks' own timestamps rather than arrival, so the
    waveform is what the device produced and a gap stays a gap.

    What a well-formed block is comes from `streamlib.AudioBlock` rather than
    being restated here: it already refuses a payload whose length disagrees
    with its declared shape, and already reads an absent dtype as `f32` the way
    the wire contract says to.
    """
    from streamlib import AudioBlock
    from streamlib._engine import decode_tapped_channel_bag_frame_to_python_object

    tapped = []
    for index, bag in enumerate(tapped_bags):
        if bag.get("hex_truncated"):
            bound = (
                f" (the tap forwards at most {preview_bound_bytes} bytes)"
                if preview_bound_bytes
                else ""
            )
            raise TappedBagWasTruncated(
                f"bag {index} is {bag.get('byte_len', 'an unstated number of')} bytes "
                f"and only a preview arrived{bound} — the block is larger than the "
                f"tap carries, so what arrived cannot be measured"
            )
        framed = bytes.fromhex(bag["hex_preview"])
        decoded = decode_tapped_channel_bag_frame_to_python_object(framed)
        if not isinstance(decoded, dict):
            raise ValueError(f"bag {index} is not a named map: {type(decoded).__name__}")
        try:
            block = AudioBlock.from_bag(decoded)
        except (ValueError, TypeError) as refusal:
            raise ValueError(f"bag {index}: {refusal}") from refusal
        tapped.append(TappedAudioBlock(block, frame_timestamp_ns(framed)))
    tapped.sort(key=lambda entry: entry.block.first_sample_timestamp_ns)
    return tapped


def continuity_errors_ms(tapped):
    """Every gap or overlap between consecutive blocks, in milliseconds.

    Each block says when its first sample was captured and how many samples it
    carries, so where the next one belongs is arithmetic. A block the source
    dropped and did not account for shows up here.
    """
    errors = []
    for earlier, later in zip(tapped, tapped[1:]):
        expected = earlier.block.first_sample_timestamp_ns + (
            earlier.block.sample_count * 1_000_000_000 // earlier.block.sample_rate
        )
        errors.append(
            (later.block.first_sample_timestamp_ns - expected) / 1_000_000.0
        )
    return errors


def one_value_across(tapped, attribute):
    """The value every block agrees on, or None where they disagree."""
    values = {getattr(entry.block, attribute) for entry in tapped}
    return values.pop() if len(values) == 1 else None


def waveform_from(tapped):
    """The published samples as one signal, each block at its own instant.

    Placed by timestamp rather than concatenated, so a block that never arrived
    leaves silence where it belonged instead of being closed up — a gap that
    closes silently is a loss the measurement can no longer see.
    """
    import numpy

    origin = tapped[0].block.first_sample_timestamp_ns
    sample_rate = tapped[0].block.sample_rate
    # From the furthest-reaching block rather than the last one: a block that
    # overruns the block after it would otherwise not fit the buffer.
    length = max(
        _sample_offset_of(entry, origin, sample_rate) + entry.block.sample_count
        for entry in tapped
    )
    waveform = numpy.zeros(length, dtype="<f8")
    for entry in tapped:
        samples = entry.block.samples
        mono = samples.mean(axis=1) if entry.block.channels > 1 else samples[:, 0]
        if entry.block.dtype == "i16":
            mono = mono / 32768.0
        at = _sample_offset_of(entry, origin, sample_rate)
        waveform[at : at + len(mono)] = mono
    return waveform


def _sample_offset_of(entry, origin_ns, sample_rate):
    return round(
        (entry.block.first_sample_timestamp_ns - origin_ns) * sample_rate / 1e9
    )


def declares_a_possible_format(tapped):
    """Whether every block declares a format audio can actually have.

    Checked before anything is graded, because a zero here does not make the
    stream merely unusual — it makes every measurement below it meaningless,
    and falling through to "no bound" would grade a self-contradicting stream
    as healthy.
    """
    return all(
        getattr(entry.block, attribute) > 0
        for entry in tapped
        for attribute in ("sample_rate", "channels", "sample_count")
    )


def report_for(tapped, tap_result=None, expect_frame_not_restamped=False):
    """What the processor published, as facts a caller can assert on."""
    if not declares_a_possible_format(tapped):
        return {
            "blocks": len(tapped),
            "failed": ["declared_format"],
            "verdict": "FAIL",
            "reason": "a block declares a sample rate, channel count or sample "
            "count of zero, which no audio has",
            "signal_measured": False,
        }
    sample_count = one_value_across(tapped, "sample_count")
    sample_rate = one_value_across(tapped, "sample_rate")
    block_ms = (
        1000.0 * sample_count / sample_rate if sample_count and sample_rate else None
    )
    errors = continuity_errors_ms(tapped)
    worst = max(errors, key=abs, default=0.0)
    dropped_by_the_tap = (tap_result or {}).get("dropped_bags", 0)

    report = {
        "blocks": len(tapped),
        "sample_rate": sample_rate,
        "channels": one_value_across(tapped, "channels"),
        "dtype": one_value_across(tapped, "dtype"),
        "sample_count": sample_count,
        "block_continuity_error_ms": round(worst, 3),
        "cumulative_continuity_error_ms": round(sum(errors), 3),
        "first_sample_timestamp_ns": tapped[0].block.first_sample_timestamp_ns,
        "frame_versus_block_timestamp_error_ns": max(
            abs(entry.frame_timestamp_ns - entry.block.first_sample_timestamp_ns)
            for entry in tapped
        ),
        "bags_dropped_by_the_tap": dropped_by_the_tap,
        # Said in every report rather than only where a waveform was written:
        # the verdict covers what the blocks declare about themselves, never
        # what they carry.
        "signal_measured": False,
    }

    failures = []
    if report["blocks"] < 2:
        failures.append("blocks")
    for attribute in ("sample_rate", "channels", "dtype", "sample_count"):
        if report[attribute] is None:
            # A stream that changed format mid-run is a defect, not a variation.
            failures.append(attribute)
    if dropped_by_the_tap:
        # The observer's own loss, which the tap reports separately. Attributing
        # it to the processor would be the opposite of what this tool is for.
        failures.append("bags_dropped_by_the_tap")
    if block_ms is not None:
        if abs(worst) > MAX_CONTINUITY_ERROR_AS_FRACTION_OF_A_BLOCK * block_ms:
            failures.append("block_continuity_error_ms")
        if abs(report["cumulative_continuity_error_ms"]) > (
            MAX_CUMULATIVE_CONTINUITY_ERROR_PER_GAP_AS_BLOCKS
            * block_ms
            * max(len(errors), 1)
        ):
            failures.append("cumulative_continuity_error_ms")
    if expect_frame_not_restamped and report["frame_versus_block_timestamp_error_ns"]:
        # Only a capture built-in carries the device's instant into the frame;
        # every other producer stamps at publication, so this is what the caller
        # asks for rather than what every channel owes.
        failures.append("frame_versus_block_timestamp_error_ns")

    report["failed"] = failures
    report["verdict"] = "PASS" if not failures else "FAIL"
    return report


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tapped_bags_json", help="what `streamlib tap` returned")
    parser.add_argument("--waveform", help="write the published samples here as WAV")
    parser.add_argument(
        "--expect-frame-not-restamped",
        action="store_true",
        help="require the frame's timestamp to match the block's own, as a capture "
        "built-in publishes and a producer that stamps at publication does not",
    )
    arguments = parser.parse_args(argv[1:])

    with open(arguments.tapped_bags_json) as tapped_bags_file:
        tap_result = json.load(tapped_bags_file)
    bags = tap_result["bags"] if isinstance(tap_result, dict) else tap_result
    if not bags:
        print(
            json.dumps(
                {"verdict": "FAIL", "reason": "the tap returned no bags"}, indent=2
            )
        )
        return 1

    tapped = audio_blocks_from_tapped_bags(bags)
    report = report_for(
        tapped,
        tap_result if isinstance(tap_result, dict) else None,
        expect_frame_not_restamped=arguments.expect_frame_not_restamped,
    )
    # Printed before the waveform is written: reassembling a self-contradicting
    # stream can fail, and the diagnosis is already computed by here — losing it
    # to a traceback is the operator's whole answer gone.
    print(json.dumps(report, indent=2))
    if arguments.waveform and report["verdict"] == "PASS":
        known_audio_signal.write_wav(
            arguments.waveform, waveform_from(tapped), report["sample_rate"]
        )
    return 0 if report["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
