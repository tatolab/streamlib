# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Controls for reading an audio channel off its tap.

Synthesised tap payloads throughout, so this needs no running node and no
device. It does need the wheel, unlike the loopback fixture beside it: the tap's
payloads are transport-framed and `streamlib.AudioBlock` is what says whether a
decoded block is well formed.
"""

import contextlib
import io
import json
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

    def report_for_bags(self, bags, **kwargs):
        return tap_audio_channel.report_for(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags), **kwargs
        )

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
        report = self.report_for_bags(self.tapped_bags())
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
        report = self.report_for_bags(bags)
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertEqual(
            report["failed"],
            ["block_continuity_error_ms", "cumulative_continuity_error_ms"],
            report,
        )
        self.assertAlmostEqual(
            report["block_continuity_error_ms"], BLOCK_DURATION_NS / 1e6, delta=0.1
        )

    def test_blocks_are_ordered_by_capture_not_by_arrival(self):
        bags = self.tapped_bags(count=4)
        bags.reverse()
        tapped = tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        stamps = [entry.block.first_sample_timestamp_ns for entry in tapped]
        self.assertEqual(stamps, sorted(stamps))
        self.assertEqual(
            tap_audio_channel.report_for(tapped)["block_continuity_error_ms"], 0.0
        )

    def test_a_stream_that_changes_format_mid_run_is_refused(self):
        """A variation here is a defect, not a variation: a consumer sized its
        reads on the first block it saw."""
        bags = self.tapped_bags(count=4, overrides={2: {"sample_rate": 44_100}})
        report = self.report_for_bags(bags)
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
        self.assertEqual(
            self.report_for_bags(bags)["verdict"],
            "PASS",
            "only a capture built-in publishes the device's own instant, so this "
            "is not something every channel owes",
        )
        report = self.report_for_bags(bags, expect_frame_not_restamped=True)
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

    def test_a_format_no_audio_could_have_is_refused_rather_than_graded(self):
        """A zero here does not make the stream unusual, it makes every
        measurement below it meaningless — and falling through to "no bound"
        would grade a stream that contradicts itself as healthy."""
        for attribute in ("sample_count", "channels", "sample_rate"):
            with self.subTest(zeroed=attribute):
                zeroed = {attribute: 0}
                if attribute == "sample_count":
                    zeroed["samples"] = b""
                elif attribute == "channels":
                    zeroed["samples"] = b""
                bags = self.tapped_bags(
                    count=3, overrides=dict.fromkeys(range(3), zeroed)
                )
                report = self.report_for_bags(bags)
                self.assertEqual(report["verdict"], "FAIL", report)
                self.assertEqual(report["failed"], ["declared_format"], report)

    def test_a_single_bag_is_not_enough_to_call_a_stream_continuous(self):
        """One block has no gap to measure, so a consistent report on it says
        nothing about cadence."""
        report = self.report_for_bags(self.tapped_bags(count=1))
        self.assertEqual(report["failed"], ["blocks"], report)

    def test_continuity_is_measured_at_the_rate_the_blocks_declare(self):
        """Not at the one every other control happens to use: the rate is the
        device's to choose, and a bound that assumes one is no bound."""
        # Far enough from the rate every other control uses that assuming it
        # produces an error larger than the bound; 16 kHz is an ordinary speech
        # rate, not a contrived one.
        quantum, rate = 160, 16_000
        duration_ns = quantum * 1_000_000_000 // rate
        bags = [
            _framed_bag(
                {
                    "samples": b"\x00" * (quantum * 4),
                    "sample_rate": rate,
                    "channels": 1,
                    "sample_count": quantum,
                    "dtype": "f32",
                    "first_sample_timestamp_ns": 1_000_000_000 + index * duration_ns,
                }
            )
            for index in range(5)
        ]
        self.assertEqual(self.report_for_bags(bags)["verdict"], "PASS")
        del bags[2]
        self.assertIn("block_continuity_error_ms", self.report_for_bags(bags)["failed"])

    def test_every_report_says_whether_a_signal_was_measured(self):
        """The verdict covers what the blocks declare about themselves, never
        what they carry, and a reader should not have to know that."""
        self.assertIs(self.report_for_bags(self.tapped_bags())["signal_measured"], False)

    def test_a_payload_that_contradicts_its_declared_shape_is_refused(self):
        """The corruption this tool exists to catch, and the one the truncation
        flag cannot see: a payload the tap delivered whole that is simply not
        the length its own header claims."""
        for label, samples in (
            ("half the declared length", b"\x00" * (SAMPLE_COUNT * 2)),
            ("empty", b""),
            ("a single byte", b"\x01"),
        ):
            with self.subTest(payload=label):
                bags = self.tapped_bags(count=2, overrides={1: {"samples": samples}})
                with self.assertRaises(ValueError) as refusal:
                    tap_audio_channel.audio_blocks_from_tapped_bags(bags)
                self.assertIn("bag 1", str(refusal.exception))

    def test_a_block_with_no_dtype_reads_as_f32(self):
        """Absent on the wire means f32, so a legal block that omits it must be
        measured rather than crash the tool."""
        bags = []
        for index in range(3):
            bags.append(
                _framed_bag(
                    {
                        "samples": b"\x00" * (SAMPLE_COUNT * 4),
                        "sample_rate": SAMPLE_RATE,
                        "channels": 1,
                        "sample_count": SAMPLE_COUNT,
                        "first_sample_timestamp_ns": 1_000_000_000
                        + index * BLOCK_DURATION_NS,
                    }
                )
            )
        report = self.report_for_bags(bags)
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertEqual(report["dtype"], "f32")

    def test_a_dropped_block_is_caught_at_a_smaller_quantum_too(self):
        """The bound is a fraction of a block's own duration, not a constant: a
        constant written for one quantum stops discriminating at another, and
        the quantum is the device's to choose."""
        for quantum in (128, 240, 512):
            with self.subTest(quantum=quantum):
                duration_ns = quantum * 1_000_000_000 // SAMPLE_RATE
                bags = [
                    _framed_bag(
                        {
                            "samples": b"\x00" * (quantum * 4),
                            "sample_rate": SAMPLE_RATE,
                            "channels": 1,
                            "sample_count": quantum,
                            "dtype": "f32",
                            "first_sample_timestamp_ns": 1_000_000_000
                            + index * duration_ns,
                        }
                    )
                    for index in range(5)
                ]
                del bags[2]
                self.assertIn(
                    "block_continuity_error_ms", self.report_for_bags(bags)["failed"]
                )

    def test_bags_the_tap_itself_dropped_are_not_blamed_on_the_processor(self):
        """The tap reports its own loss separately, and attributing it to the
        source would be the opposite of what this tool is for."""
        report = tap_audio_channel.report_for(
            tap_audio_channel.audio_blocks_from_tapped_bags(self.tapped_bags()),
            {"dropped_bags": 3},
        )
        self.assertEqual(report["failed"], ["bags_dropped_by_the_tap"], report)
        self.assertEqual(report["bags_dropped_by_the_tap"], 3)

    def test_a_bag_that_is_not_an_audio_block_is_named(self):
        bags = self.tapped_bags(count=1)
        bags.append(_framed_bag({"surface_id": "42", "width": 8}))
        with self.assertRaises(ValueError) as refusal:
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        self.assertIn("samples", str(refusal.exception))

    def test_a_stereo_block_is_downmixed_rather_than_read_from_one_channel(self):
        """Identical channels cannot tell a downmix apart from taking channel
        zero, so this carries a different level in each."""
        loud_left_silent_right = numpy.array(
            [1.0, 0.0] * SAMPLE_COUNT, dtype="<f4"
        ).tobytes()
        bags = [
            _framed_bag(
                {
                    "samples": loud_left_silent_right,
                    "sample_rate": SAMPLE_RATE,
                    "channels": 2,
                    "sample_count": SAMPLE_COUNT,
                    "dtype": "f32",
                    "first_sample_timestamp_ns": 1_000_000_000
                    + index * BLOCK_DURATION_NS,
                }
            )
            for index in range(2)
        ]
        waveform = tap_audio_channel.waveform_from(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        )
        self.assertAlmostEqual(
            float(waveform.max()),
            0.5,
            places=4,
            msg="the mean of a loud and a silent channel, not the loud one alone",
        )

    def test_a_gap_is_left_as_silence_rather_than_closed_up(self):
        """Blocks are placed by their own timestamps, not concatenated. A gap
        that closes silently is a loss the measurement can no longer see, and
        the waveform stops being what the device produced.

        Each block carries its own level, because all-zero payloads cannot tell
        placement apart from concatenation — the length is right either way.
        """
        bags = [_block_at_level((index + 1) / 10.0, index) for index in range(5)]
        del bags[2]
        waveform = tap_audio_channel.waveform_from(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        )

        self.assertEqual(
            len(waveform),
            5 * SAMPLE_COUNT,
            "the timeline the surviving blocks declare spans all five blocks",
        )
        self.assertAlmostEqual(
            float(waveform[2 * SAMPLE_COUNT + 10]),
            0.0,
            places=4,
            msg="the block that never arrived leaves silence where it belonged",
        )
        self.assertAlmostEqual(
            float(waveform[3 * SAMPLE_COUNT + 10]),
            0.4,
            places=4,
            msg="the block after the gap sits at its own instant, not shifted early",
        )

    def test_a_block_overrunning_the_next_one_still_reassembles(self):
        """The buffer is sized from the furthest-reaching block, not the last
        one — otherwise a malformed stream crashes the reassembly instead of
        being reported."""
        bags = [
            _block_at_level(0.5, 0, sample_count=SAMPLE_COUNT * 4),
            _block_at_level(0.5, 1),
        ]
        waveform = tap_audio_channel.waveform_from(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        )
        self.assertEqual(len(waveform), SAMPLE_COUNT * 4)

    def test_blocks_that_overlap_in_time_are_reported(self):
        """An overlap is as much a break in continuity as a gap: two blocks
        cannot both have been captured at the same instant."""
        bags = self.tapped_bags(count=4)
        bags[3] = _framed_bag(
            {
                "samples": b"\x00" * (SAMPLE_COUNT * 4),
                "sample_rate": SAMPLE_RATE,
                "channels": 1,
                "sample_count": SAMPLE_COUNT,
                "dtype": "f32",
                "first_sample_timestamp_ns": 1_000_000_000 + BLOCK_DURATION_NS,
            }
        )
        report = self.report_for_bags(bags)
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("block_continuity_error_ms", report["failed"])
        self.assertLess(report["block_continuity_error_ms"], 0.0)

    def test_the_command_exits_non_zero_and_prints_the_failures(self):
        """The exit status is what `verify_audio_channel.sh` gates on, and the
        JSON is what a reader acts on — neither is exercised by calling
        `report_for` directly."""
        bags = self.tapped_bags(count=6)
        del bags[2]
        tapped_json = Path(self.workspace.name) / "tapped.json"
        tapped_json.write_text(json.dumps({"bags": bags, "dropped_bags": 0}))

        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            exit_status = tap_audio_channel.main(
                ["tap_audio_channel.py", str(tapped_json)]
            )

        self.assertEqual(exit_status, 1)
        report = json.loads(printed.getvalue())
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("block_continuity_error_ms", report["failed"])

    def test_the_published_samples_reassemble_into_a_measurable_waveform(self):
        """The point of the whole tool: what the processor published goes
        through the same measurement a loopback capture does."""
        signal = known_audio_signal.generate_signal()
        bags = _bags_carrying(signal)
        waveform = tap_audio_channel.waveform_from(
            tap_audio_channel.audio_blocks_from_tapped_bags(bags)
        )

        captured = Path(self.workspace.name) / "tapped.wav"
        known_audio_signal.write_wav(str(captured), waveform, SAMPLE_RATE)
        report = known_audio_signal.analyse(
            str(captured), str(Path(self.workspace.name) / "spectrogram.png")
        )
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertEqual(report["symbols"], known_audio_signal.DTMF_DIGITS)

    def test_the_signal_survives_i16_and_stereo_the_same_as_mono_f32(self):
        """dtype and channel count are the device's to choose, and the cast's
        little-endian spelling is a wire contract rather than a platform
        assumption — so the same signal has to come back out of all of them."""
        signal = known_audio_signal.generate_signal()
        for label, bags in (
            ("i16 mono", _bags_carrying(signal, dtype="i16")),
            ("f32 stereo", _bags_carrying(signal, channels=2)),
        ):
            with self.subTest(carried_as=label):
                captured = Path(self.workspace.name) / "tapped.wav"
                known_audio_signal.write_wav(
                    str(captured),
                    tap_audio_channel.waveform_from(
                        tap_audio_channel.audio_blocks_from_tapped_bags(bags)
                    ),
                    SAMPLE_RATE,
                )
                report = known_audio_signal.analyse(
                    str(captured), str(Path(self.workspace.name) / "spectrogram.png")
                )
                # The verdict, not just the symbols: identity survives gross
                # corruption by design, so a control that stops there would
                # bless a 32768x amplitude error as a clean read.
                self.assertEqual(report["verdict"], "PASS", report)

    def test_a_processor_that_dropped_a_block_fails_the_signal_measurement_too(self):
        """The two halves agree: a block missing from the published stream is a
        break in continuity AND audio missing from the waveform."""
        signal = known_audio_signal.generate_signal()
        bags = _bags_carrying(signal)
        del bags[len(bags) // 2]
        tapped = tap_audio_channel.audio_blocks_from_tapped_bags(bags)

        self.assertIn(
            "block_continuity_error_ms", tap_audio_channel.report_for(tapped)["failed"]
        )
        captured = Path(self.workspace.name) / "tapped.wav"
        known_audio_signal.write_wav(
            str(captured), tap_audio_channel.waveform_from(tapped), SAMPLE_RATE
        )
        self.assertEqual(
            known_audio_signal.analyse(
                str(captured), str(Path(self.workspace.name) / "spectrogram.png")
            )["verdict"],
            "FAIL",
        )


def _block_at_level(level, index, sample_count=SAMPLE_COUNT):
    """One block carrying a constant level, so placement is distinguishable."""
    return _framed_bag(
        {
            "samples": numpy.full(sample_count, level, dtype="<f4").tobytes(),
            "sample_rate": SAMPLE_RATE,
            "channels": 1,
            "sample_count": sample_count,
            "dtype": "f32",
            "first_sample_timestamp_ns": 1_000_000_000 + index * BLOCK_DURATION_NS,
        }
    )


def _bags_carrying(signal, dtype="f32", channels=1):
    """The signal cut into device-quantum blocks, as a tap would forward them."""
    bags = []
    for index in range(len(signal) // SAMPLE_COUNT):
        quantum = signal[index * SAMPLE_COUNT : (index + 1) * SAMPLE_COUNT]
        if channels > 1:
            quantum = numpy.repeat(quantum, channels)
        if dtype == "i16":
            payload = (quantum * 32767.0).astype("<i2").tobytes()
        else:
            payload = quantum.astype("<f4").tobytes()
        bags.append(
            _framed_bag(
                {
                    "samples": payload,
                    "sample_rate": SAMPLE_RATE,
                    "channels": channels,
                    "sample_count": SAMPLE_COUNT,
                    "dtype": dtype,
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
    encoded_bytes = bytearray()
    if len(mapping) < 16:
        encoded_bytes.append(0x80 | len(mapping))
    else:
        encoded_bytes.extend(b"\xde" + struct.pack(">H", len(mapping)))
    for key, value in mapping.items():
        encoded_bytes.extend(_msgpack_value(key))
        encoded_bytes.extend(_msgpack_value(value))
    return bytes(encoded_bytes)


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
