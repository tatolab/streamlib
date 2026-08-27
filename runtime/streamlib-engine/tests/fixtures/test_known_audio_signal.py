# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Negative controls for the audio loopback fixture's analysis half.

A fixture that cannot go red is worth nothing, so these are the tests that
carry it: each corrupts a clean signal in one known failure mode and asserts
the report fails on that axis and no other. Synthesised end to end, so this
needs no audio device, no session, and no engine — which is what lets the
measurement half be checked everywhere the loopback itself cannot run.
"""

import tempfile
import unittest
from pathlib import Path

import numpy

import known_audio_signal as fixture

# Where the corruptions are applied, measured from the start of the signal.
# Inside the digit run, so a shortened interval is attributable to a digit pair
# rather than to the lead-in.
A_POINT_INSIDE_THE_DIGITS_SECONDS = 1.5
A_DROPPED_BLOCK_SECONDS = 0.04
A_HOLE_LONGER_THAN_ANY_GAP_SECONDS = 0.30


class KnownAudioSignalAnalysis(unittest.TestCase):
    def setUp(self):
        self.workspace = tempfile.TemporaryDirectory()
        self.addCleanup(self.workspace.cleanup)
        self.clean = fixture.generate_signal()

    def report_for(self, samples):
        captured = Path(self.workspace.name) / "captured.wav"
        fixture.write_wav(str(captured), samples)
        return fixture.analyse(
            str(captured), str(Path(self.workspace.name) / "spectrogram.png")
        )

    def test_a_clean_signal_passes_every_axis(self):
        report = self.report_for(self.clean)
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertEqual(report["failed"], [])
        self.assertEqual(report["symbols"], fixture.DTMF_DIGITS)

    def test_a_dropped_block_fails_on_timing_and_names_where_it_happened(self):
        """The failure mode the whole signal design exists for.

        A tone survives a dropped block almost invisibly — the fundamental is
        unchanged — and so does a symbol's identity, because clipping part of a
        digit still decodes as that digit. Only the interval between digits
        moves, and it moves by exactly the audio that went missing.
        """
        cut_at = int(A_POINT_INSIDE_THE_DIGITS_SECONDS * fixture.SAMPLE_RATE)
        dropped = int(A_DROPPED_BLOCK_SECONDS * fixture.SAMPLE_RATE)
        report = self.report_for(
            numpy.concatenate([self.clean[:cut_at], self.clean[cut_at + dropped :]])
        )

        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertEqual(report["failed"], ["symbol_interval_error_ms"])
        self.assertEqual(
            report["symbols"],
            fixture.DTMF_DIGITS,
            "identity alone survives the loss, which is why it cannot be the check",
        )
        self.assertAlmostEqual(
            report["symbol_interval_error_ms"],
            -A_DROPPED_BLOCK_SECONDS * 1000.0,
            delta=5.0,
        )
        self.assertIsNotNone(
            report["worst_symbol_interval"], "the report has to say where"
        )

    def test_a_gain_error_fails_on_amplitude_alone(self):
        report = self.report_for(self.clean * 0.6)
        self.assertEqual(report["failed"], ["amplitude"], report)

    def test_a_sample_rate_mismatch_fails_on_frequency_and_symbols(self):
        """Samples captured at one rate and read as another: every frequency in
        the signal shifts, so both the tone and the digits decode wrong."""
        misread = numpy.interp(
            numpy.arange(0, len(self.clean), 44_100 / 48_000),
            numpy.arange(len(self.clean)),
            self.clean,
        )
        report = self.report_for(misread)
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("fundamental_hz", report["failed"])
        self.assertIn("symbols", report["failed"])

    def test_a_hole_punched_in_the_audio_is_counted_as_a_gap(self):
        holed = self.clean.copy()
        hole_at = int(0.5 * fixture.SAMPLE_RATE)
        holed[hole_at : hole_at + int(A_HOLE_LONGER_THAN_ANY_GAP_SECONDS * fixture.SAMPLE_RATE)] = 0.0
        report = self.report_for(holed)
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("gap_count", report["failed"])
        self.assertGreater(report["gap_count"], 0)

    def test_silence_is_refused_rather_than_measured(self):
        report = self.report_for(numpy.zeros_like(self.clean))
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("silent", report["reason"])

    def test_the_spectrogram_is_a_readable_png(self):
        """The half a human and a session judge by eye — a report with an
        unopenable image is a report with no evidence in it."""
        spectrogram = Path(self.workspace.name) / "spectrogram.png"
        self.report_for(self.clean)
        header = spectrogram.read_bytes()[:8]
        self.assertEqual(header, b"\x89PNG\r\n\x1a\n")
        self.assertGreater(spectrogram.stat().st_size, 1024)


if __name__ == "__main__":
    unittest.main()
