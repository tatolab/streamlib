# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Controls for reading an audio channel off its tap.

Synthesised tap payloads throughout, so this needs no running node, no device
and no engine — the same reason the loopback fixture's analysis half is
checkable everywhere the loopback itself cannot run.
"""

import struct
import tempfile
import unittest
from pathlib import Path

import numpy

import known_audio_signal
import tap_audio_channel

SAMPLE_RATE = 48_000
SAMPLE_COUNT = 512
BLOCK_DURATION_NS = SAMPLE_COUNT * 1_000_000_000 // SAMPLE_RATE


class TappedAudioChannel(unittest.TestCase):
    def setUp(self):
        self.workspace = tempfile.TemporaryDirectory()
        self.addCleanup(self.workspace.cleanup)

    def tapped_bags(self, count=4, first_timestamp_ns=1_000_000_000, overrides=None):
        bags = []
        for index in range(count):
            block = {
                "samples": b"\x00" * (SAMPLE_COUNT * 4),
                "sample_rate": SAMPLE_RATE,
                "channels": 1,
                "sample_count": SAMPLE_COUNT,
                "dtype": "f32",
                "first_sample_timestamp_ns": first_timestamp_ns
                + index * BLOCK_DURATION_NS,
            }
            block.update((overrides or {}).get(index, {}))
            bags.append(_framed_bag(block))
        return bags

    def test_a_healthy_stream_reports_its_format_and_cadence(self):
        report = tap_audio_channel.report_for(
            tap_audio_channel.audio_blocks_from_tapped_bags(self.tapped_bags())
        )
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertEqual(report["blocks"], 4)
        self.assertEqual(report["sample_rate"], SAMPLE_RATE)
        self.assertEqual(report["channels"], 1)
        self.assertEqual(report["dtype"], "f32")
        self.assertEqual(report["block_continuity_error_ms"], 0.0)

    def test_a_block_the_source_dropped_shows_as_a_break_in_continuity(self):
        """Each block says when it started and how many samples it carries, so
        where the next one belongs is arithmetic — and a block that never
        arrived leaves exactly its own duration of daylight."""
        bags = self.tapped_bags(count=4)
        del bags[2]
        report = tap_audio_channel.report_for(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        )
        self.assertEqual(report["failed"], ["block_continuity_error_ms"], report)
        self.assertAlmostEqual(
            report["block_continuity_error_ms"], BLOCK_DURATION_NS / 1e6, delta=0.1
        )

    def test_blocks_are_ordered_by_capture_not_by_arrival(self):
        bags = self.tapped_bags(count=4)
        bags.reverse()
        blocks = tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        stamps = [block["first_sample_timestamp_ns"] for block in blocks]
        self.assertEqual(stamps, sorted(stamps))
        self.assertEqual(
            tap_audio_channel.report_for(blocks)["block_continuity_error_ms"], 0.0
        )

    def test_a_stream_that_changes_format_mid_run_is_refused(self):
        """A variation here is a defect, not a variation: a consumer sized its
        reads on the first block it saw."""
        bags = self.tapped_bags(count=4, overrides={2: {"sample_rate": 44_100}})
        report = tap_audio_channel.report_for(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        )
        self.assertIn("sample_rate", report["failed"], report)

    def test_a_frame_stamped_at_publication_rather_than_capture_is_caught(self):
        """The frame header and the block are written from one value, so any
        difference means something re-stamped the frame when it published —
        naming the instant it was sent rather than the instant it was heard."""
        bags = self.tapped_bags(count=3)
        bags[1] = _framed_bag(
            {
                "samples": b"\x00" * (SAMPLE_COUNT * 4),
                "sample_rate": SAMPLE_RATE,
                "channels": 1,
                "sample_count": SAMPLE_COUNT,
                "dtype": "f32",
                "first_sample_timestamp_ns": 1_000_000_000 + BLOCK_DURATION_NS,
            },
            frame_timestamp_ns=1_000_000_000 + BLOCK_DURATION_NS + 4_000_000,
        )
        report = tap_audio_channel.report_for(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        )
        self.assertEqual(
            report["failed"], ["frame_versus_block_timestamp_error_ns"], report
        )
        self.assertEqual(report["frame_versus_block_timestamp_error_ns"], 4_000_000)

    def test_a_truncated_bag_is_refused_rather_than_measured(self):
        """The tap forwards a bounded preview. Measuring what arrived would
        produce a confident wrong answer about the samples that did not."""
        bags = self.tapped_bags(count=2)
        bags[1]["hex_truncated"] = True
        with self.assertRaises(tap_audio_channel.TappedBagWasTruncated) as refusal:
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        self.assertIn("preview", str(refusal.exception))

    def test_a_bag_that_is_not_an_audio_block_is_named(self):
        bags = self.tapped_bags(count=1)
        bags.append(_framed_bag({"surface_id": "42", "width": 8}))
        with self.assertRaises(ValueError) as refusal:
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        self.assertIn("samples", str(refusal.exception))

    def test_the_published_samples_reassemble_into_a_measurable_waveform(self):
        """The point of the whole tool: what the processor published goes
        through the same measurement a loopback capture does."""
        signal = known_audio_signal.generate_signal()
        bags = _bags_carrying(signal)
        blocks = tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        waveform = tap_audio_channel.waveform_from(blocks)

        captured = Path(self.workspace.name) / "tapped.wav"
        known_audio_signal.write_wav(str(captured), waveform, SAMPLE_RATE)
        report = known_audio_signal.analyse(
            str(captured), str(Path(self.workspace.name) / "spectrogram.png")
        )
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertEqual(report["symbols"], known_audio_signal.DTMF_DIGITS)

    def test_a_processor_that_dropped_a_block_fails_the_signal_measurement_too(self):
        """The two halves agree: a block missing from the published stream is a
        break in continuity AND audio missing from the waveform."""
        signal = known_audio_signal.generate_signal()
        bags = _bags_carrying(signal)
        del bags[len(bags) // 2]
        blocks = tap_audio_channel.audio_blocks_from_tapped_bags(bags)

        self.assertIn(
            "block_continuity_error_ms", tap_audio_channel.report_for(blocks)["failed"]
        )
        captured = Path(self.workspace.name) / "tapped.wav"
        known_audio_signal.write_wav(
            str(captured), tap_audio_channel.waveform_from(blocks), SAMPLE_RATE
        )
        self.assertEqual(
            known_audio_signal.analyse(
                str(captured), str(Path(self.workspace.name) / "spectrogram.png")
            )["verdict"],
            "FAIL",
        )


def _bags_carrying(signal):
    """The signal cut into device-quantum blocks, as a tap would forward them."""
    bags = []
    for index in range(len(signal) // SAMPLE_COUNT):
        quantum = signal[index * SAMPLE_COUNT : (index + 1) * SAMPLE_COUNT]
        bags.append(
            _framed_bag(
                {
                    "samples": quantum.astype("<f4").tobytes(),
                    "sample_rate": SAMPLE_RATE,
                    "channels": 1,
                    "sample_count": SAMPLE_COUNT,
                    "dtype": "f32",
                    "first_sample_timestamp_ns": 1_000_000_000
                    + index * BLOCK_DURATION_NS,
                }
            )
        )
    return bags


def _framed_bag(block, frame_timestamp_ns=None):
    """One bag as the tap hands it over: the transport frame, then msgpack, hex."""
    payload = _msgpack_named_map(block)
    stamp = (
        block.get("first_sample_timestamp_ns", 0)
        if frame_timestamp_ns is None
        else frame_timestamp_ns
    )
    port_key = bytearray(64)
    name = b"audio"
    port_key[0] = len(name)
    port_key[1 : 1 + len(name)] = name
    encoded = (
        bytes(port_key)
        + struct.pack("<q", stamp)
        + struct.pack("<I", len(payload))
        + payload
    )
    return {
        "byte_len": len(encoded),
        "hex_preview": encoded.hex(),
        "hex_truncated": False,
    }


def _msgpack_named_map(mapping):
    """A minimal msgpack encoder, so these controls need no packing library."""
    out = bytearray()
    out.append(0x80 | len(mapping)) if len(mapping) < 16 else out.extend(
        b"\xde" + struct.pack(">H", len(mapping))
    )
    for key, value in mapping.items():
        out.extend(_msgpack_value(key))
        out.extend(_msgpack_value(value))
    return bytes(out)


def _msgpack_value(value):
    if isinstance(value, str):
        raw = value.encode()
        return bytes([0xA0 | len(raw)]) + raw if len(raw) < 32 else (
            b"\xd9" + bytes([len(raw)]) + raw
        )
    if isinstance(value, bytes):
        return b"\xc6" + struct.pack(">I", len(value)) + value
    if isinstance(value, bool):
        return b"\xc3" if value else b"\xc2"
    if isinstance(value, int):
        if 0 <= value < 128:
            return bytes([value])
        return b"\xd3" + struct.pack(">q", value)
    raise TypeError(f"{type(value).__name__} is not encoded here")


if __name__ == "__main__":
    unittest.main()
