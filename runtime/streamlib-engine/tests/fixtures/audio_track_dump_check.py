#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Read what an audio processor published, off its own output port's track.

Verifying audio should not need a loopback any more than verifying a frame
needs a display window. An audio block carries its samples inline, so the
port's track at the local relay already carries everything needed to measure
what a processor produced — per processor, with no second device in the path.

It reads the directory `tatolab-read-track` wrote of the port's track: each
object's payload, one msgpack `AudioBlock` as the port wrote it, and the
timestamp the publisher stamped it with. `tatolab.stream.AudioBlock` is the
cast that says what a well-formed block is, so this needs `tatolab-stream` and
no runtime.

Two things come back. What can be checked with no signal at all: block cadence,
timestamp continuity, rate, channels, dtype — over the real IPC and relay path
rather than a test harness. And the samples themselves, placed by their own
timestamps into a waveform that `known_audio_signal.py analyse` measures
exactly as it measures a loopback capture.
"""

import argparse
import json
import sys
from pathlib import Path
from typing import NamedTuple, Optional

import known_audio_signal

# How far a block may sit from where the one before it predicted, as a fraction
# of a block's own duration. A fraction rather than a constant because the
# quantum is the device's to choose: a bound written for 512 samples at 48 kHz
# stops discriminating entirely at 128.
MAX_CONTINUITY_ERROR_AS_FRACTION_OF_A_BLOCK = 0.5

# Loss spread thinly stays under the per-gap bound while still adding up. Scaled
# by the number of gaps, so a longer read is not a laxer one: a fixed budget
# makes sensitivity a function of how many objects were asked for.
MAX_CUMULATIVE_CONTINUITY_ERROR_PER_GAP_AS_BLOCKS = 0.15


class AudioTrackObject(NamedTuple):
    """One object of the track as `tatolab-read-track` wrote it."""

    payload: bytes
    # The publisher's stamp — the port's own write stamp — or None when the
    # object carried none.
    timestamp_ns: Optional[int]


def audio_track_objects_in(dump_directory):
    """The objects `tatolab-read-track` wrote into `dump_directory`, in the
    order it read them."""
    dump_directory = Path(dump_directory)
    objects = []
    for line in (dump_directory / "objects.jsonl").read_text().splitlines():
        indexed = json.loads(line)
        objects.append(
            AudioTrackObject(
                (dump_directory / indexed["file"]).read_bytes(), indexed["timestamp_ns"]
            )
        )
    return objects


class AudioBlockReadOffItsTrack(NamedTuple):
    """One published block, with the stamp its track carried."""

    block: object
    track_timestamp_ns: Optional[int]


def audio_blocks_from_the_track_objects(track_objects):
    """Decode the track's objects into audio blocks, arrival order ignored.

    Ordering comes from the blocks' own timestamps rather than arrival, so the
    waveform is what the device produced and a gap stays a gap.

    What a well-formed block is comes from `tatolab.stream.AudioBlock` rather than
    being restated here: it already refuses a payload whose length disagrees
    with its declared shape, and already reads an absent dtype as `f32` the way
    the wire contract says to.
    """
    from tatolab.stream import AudioBlock

    read = []
    for index, track_object in enumerate(track_objects):
        try:
            block = AudioBlock.from_bytes(track_object.payload)
        except (ValueError, TypeError) as refusal:
            raise ValueError(f"object {index}: {refusal}") from refusal
        read.append(AudioBlockReadOffItsTrack(block, track_object.timestamp_ns))
    read.sort(key=lambda entry: entry.block.first_sample_timestamp_ns)
    return read


def continuity_errors_ms(read):
    """Every gap or overlap between consecutive blocks, in milliseconds.

    Each block says when its first sample was captured and how many samples it
    carries, so where the next one belongs is arithmetic. A block the source
    dropped and did not account for shows up here.
    """
    errors = []
    for earlier, later in zip(read, read[1:]):
        expected = earlier.block.first_sample_timestamp_ns + (
            earlier.block.sample_count * 1_000_000_000 // earlier.block.sample_rate
        )
        errors.append(
            (later.block.first_sample_timestamp_ns - expected) / 1_000_000.0
        )
    return errors


def one_value_across(read, attribute):
    """The value every block agrees on, or None where they disagree."""
    values = {getattr(entry.block, attribute) for entry in read}
    return values.pop() if len(values) == 1 else None


def waveform_from(read):
    """The published samples as one signal, each block at its own instant.

    Placed by timestamp rather than concatenated, so a block that never arrived
    leaves silence where it belonged instead of being closed up — a gap that
    closes silently is a loss the measurement can no longer see.
    """
    import numpy

    origin = read[0].block.first_sample_timestamp_ns
    sample_rate = read[0].block.sample_rate
    # From the furthest-reaching block rather than the last one: a block that
    # overruns the block after it would otherwise not fit the buffer.
    length = max(
        _sample_offset_of(entry, origin, sample_rate) + entry.block.sample_count
        for entry in read
    )
    waveform = numpy.zeros(length, dtype="<f8")
    for entry in read:
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


def declares_a_possible_format(read):
    """Whether every block declares a format audio can actually have.

    Checked before anything is graded, because a zero here does not make the
    stream merely unusual — it makes every measurement below it meaningless,
    and falling through to "no bound" would grade a self-contradicting stream
    as healthy.
    """
    return all(
        getattr(entry.block, attribute) > 0
        for entry in read
        for attribute in ("sample_rate", "channels", "sample_count")
    )


def report_for(read, expect_the_track_stamped_with_the_block=False):
    """What the processor published, as facts a caller can assert on."""
    if not declares_a_possible_format(read):
        return {
            "blocks": len(read),
            "failed": ["declared_format"],
            "verdict": "FAIL",
            "reason": "a block declares a sample rate, channel count or sample "
            "count of zero, which no audio has",
            "signal_measured": False,
        }
    sample_count = one_value_across(read, "sample_count")
    sample_rate = one_value_across(read, "sample_rate")
    block_ms = (
        1000.0 * sample_count / sample_rate if sample_count and sample_rate else None
    )
    errors = continuity_errors_ms(read)
    worst = max(errors, key=abs, default=0.0)

    report = {
        "blocks": len(read),
        "sample_rate": sample_rate,
        "channels": one_value_across(read, "channels"),
        "dtype": one_value_across(read, "dtype"),
        "sample_count": sample_count,
        "block_continuity_error_ms": round(worst, 3),
        "cumulative_continuity_error_ms": round(sum(errors), 3),
        "first_sample_timestamp_ns": read[0].block.first_sample_timestamp_ns,
        # None when an object carried no stamp, which no block's own matches.
        "track_versus_block_timestamp_error_ns": max(
            (
                None
                if entry.track_timestamp_ns is None
                else abs(entry.track_timestamp_ns - entry.block.first_sample_timestamp_ns)
                for entry in read
            ),
            key=lambda error: float("inf") if error is None else error,
        ),
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
    if block_ms is not None:
        if abs(worst) > MAX_CONTINUITY_ERROR_AS_FRACTION_OF_A_BLOCK * block_ms:
            failures.append("block_continuity_error_ms")
        if abs(report["cumulative_continuity_error_ms"]) > (
            MAX_CUMULATIVE_CONTINUITY_ERROR_PER_GAP_AS_BLOCKS
            * block_ms
            * max(len(errors), 1)
        ):
            failures.append("cumulative_continuity_error_ms")
    if (
        expect_the_track_stamped_with_the_block
        and report["track_versus_block_timestamp_error_ns"] != 0
    ):
        # Only a capture built-in carries the device's instant into the message
        # stamp the track carries; every other producer stamps at publication,
        # so this is what the caller asks for rather than what every port owes.
        failures.append("track_versus_block_timestamp_error_ns")

    report["failed"] = failures
    report["verdict"] = "PASS" if not failures else "FAIL"
    return report


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "track_dump_directory", help="what `tatolab-read-track` wrote of the port's track"
    )
    parser.add_argument("--waveform", help="write the published samples here as WAV")
    parser.add_argument(
        "--expect-the-track-stamped-with-the-block",
        action="store_true",
        help="require each object's stamp to match its block's own, as a capture "
        "built-in publishes and a producer that stamps at publication does not",
    )
    arguments = parser.parse_args(argv[1:])

    track_objects = audio_track_objects_in(arguments.track_dump_directory)
    if not track_objects:
        print(
            json.dumps(
                {"verdict": "FAIL", "reason": "the track dump holds no objects"}, indent=2
            )
        )
        return 1

    read = audio_blocks_from_the_track_objects(track_objects)
    report = report_for(
        read,
        expect_the_track_stamped_with_the_block=arguments.expect_the_track_stamped_with_the_block,
    )
    # Printed before the waveform is written: reassembling a self-contradicting
    # stream can fail, and the diagnosis is already computed by here — losing it
    # to a traceback is the operator's whole answer gone.
    print(json.dumps(report, indent=2))
    if arguments.waveform and report["verdict"] == "PASS":
        known_audio_signal.write_wav(
            arguments.waveform, waveform_from(read), report["sample_rate"]
        )
    return 0 if report["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
