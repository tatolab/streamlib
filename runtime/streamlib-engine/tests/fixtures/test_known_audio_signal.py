# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Negative controls for the audio loopback fixture's analysis half.

A fixture that cannot go red is worth nothing, so these are the tests that
carry it: each corrupts a clean signal in one known failure mode and asserts
the report fails on that axis, names where, and stays quiet on the rest.

Synthesised end to end, so this needs no audio device, no session and no
engine — which is what lets the measurement half be checked everywhere the
loopback itself cannot run.
"""

import sys
import tempfile
import unittest
from pathlib import Path

import numpy

import known_audio_signal as fixture

# One device quantum at 48 kHz is ~10.7 ms — the loss a single dropped block
# costs, and the smallest this fixture claims to catch.
A_DEVICE_QUANTUM_MS = 10.7
BELOW_ANY_REAL_LOSS_MS = 2.0


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

    def signal_missing(self, milliseconds, at_seconds):
        cut = int(at_seconds * fixture.SAMPLE_RATE)
        lost = int(milliseconds / 1000.0 * fixture.SAMPLE_RATE)
        return numpy.concatenate([self.clean[:cut], self.clean[cut + lost :]])

    # ---- the clean case -----------------------------------------------------

    def test_a_clean_signal_passes_every_axis(self):
        report = self.report_for(self.clean)
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertEqual(report["failed"], [])
        self.assertEqual(report["symbols"], fixture.DTMF_DIGITS)

    # ---- the loss the whole signal design exists for ------------------------

    def test_a_dropped_block_inside_a_digit_is_caught_and_the_span_named(self):
        """Identity survives the loss and the fundamental does not move; only
        the span between two onsets shortens, and by exactly what went missing."""
        report = self.report_for(self.signal_missing(20.0, at_seconds=1.85))

        self.assertEqual(report["failed"], ["symbol_interval_error_ms"], report)
        self.assertEqual(
            report["symbols"],
            fixture.DTMF_DIGITS,
            "identity alone survives the loss, which is why it cannot be the check",
        )
        self.assertAlmostEqual(report["symbol_interval_error_ms"], -20.0, delta=2.0)
        self.assertEqual(report["worst_symbol_interval"], "2->9")

    def test_a_dropped_block_inside_the_reference_tone_is_caught_too(self):
        """The tone is half the signal's duration, so the span from its own
        onset to the first digit has to be guarded like every other span —
        otherwise a loss here is either missed or blamed on a digit pair that
        lost nothing."""
        report = self.report_for(self.signal_missing(20.0, at_seconds=0.8))

        self.assertIn("symbol_interval_error_ms", report["failed"])
        self.assertAlmostEqual(report["symbol_interval_error_ms"], -20.0, delta=2.0)
        self.assertEqual(
            report["worst_symbol_interval"],
            f"{fixture.REFERENCE_TONE_LANDMARK}->{fixture.DTMF_DIGITS[0]}",
        )

    def test_a_loss_of_one_device_quantum_is_caught(self):
        """The bar the fixture is built to clear: one dropped block, not three."""
        report = self.report_for(
            self.signal_missing(A_DEVICE_QUANTUM_MS, at_seconds=1.85)
        )
        self.assertIn("symbol_interval_error_ms", report["failed"], report)

    def test_jitter_smaller_than_any_real_loss_does_not_fail(self):
        """The other half of a usable threshold: it cannot cry wolf, or the rig
        run flakes and the gate stops being believed."""
        report = self.report_for(
            self.signal_missing(BELOW_ANY_REAL_LOSS_MS, at_seconds=1.85)
        )
        self.assertEqual(report["verdict"], "PASS", report)

    # ---- one axis each ------------------------------------------------------

    def test_a_gain_error_fails_on_amplitude_alone(self):
        report = self.report_for(self.clean * 0.6)
        self.assertEqual(report["failed"], ["amplitude"], report)

    def test_distortion_fails_on_thd_alone(self):
        """Hard clipping at a drive that leaves RMS where it was, so the tone is
        the same loudness and only its shape is wrong."""
        tone = slice(
            int(0.5 * fixture.SAMPLE_RATE), int(1.1 * fixture.SAMPLE_RATE)
        )
        clipped = numpy.clip(self.clean * 2.0, -0.55, 0.55)
        # Rescaled so the tone is the same loudness it was and only its shape
        # differs — otherwise this control fails on amplitude too and proves
        # nothing about distortion.
        clipped *= numpy.sqrt(numpy.mean(self.clean[tone] ** 2)) / numpy.sqrt(
            numpy.mean(clipped[tone] ** 2)
        )
        report = self.report_for(clipped)
        self.assertEqual(report["failed"], ["thd_percent"], report)

    def test_a_sample_rate_mismatch_fails_on_frequency_and_symbols(self):
        """Samples captured at one rate and read as another: every frequency in
        the signal shifts, so the tone and the digits both decode wrong."""
        misread = numpy.interp(
            numpy.arange(0, len(self.clean), 44_100 / 48_000),
            numpy.arange(len(self.clean)),
            self.clean,
        )
        report = self.report_for(misread)
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("fundamental_hz", report["failed"])
        self.assertIn("symbols", report["failed"])

    def test_a_hole_punched_in_the_audio_fails_on_the_gap_count_alone(self):
        """Placed clear of the window the tone is measured in, so a hole reads
        as a hole rather than also dragging the amplitude down."""
        holed = self.clean.copy()
        hole_at = int(0.32 * fixture.SAMPLE_RATE)
        holed[hole_at : hole_at + int(0.16 * fixture.SAMPLE_RATE)] = 0.0
        report = self.report_for(holed)
        self.assertEqual(report["failed"], ["gap_count"], report)
        self.assertGreater(report["gap_count"], 0)

    def test_silence_is_refused_rather_than_measured(self):
        report = self.report_for(numpy.zeros_like(self.clean))
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("silent", report["reason"])

    # ---- the properties the fixture exists for ------------------------------

    def test_a_stray_pop_before_the_tone_does_not_mis_anchor_the_analysis(self):
        """Onset is the origin every window is measured from, so anchoring on
        one loud sample would turn a click into a confident failure on axes
        that have nothing to do with it."""
        popped = self.clean.copy()
        pop_at = int(0.10 * fixture.SAMPLE_RATE)
        popped[pop_at : pop_at + int(0.001 * fixture.SAMPLE_RATE)] = 0.3
        report = self.report_for(popped)
        self.assertEqual(report["verdict"], "PASS", report)

    def test_nothing_here_pulls_in_the_engine(self):
        """Runtime independence is the fixture's reason to exist rather than a
        demo app: it has to run and report when StreamLib will not build."""
        self.assertNotIn("streamlib", sys.modules)

    def test_the_spectrogram_is_a_readable_png(self):
        """The half a human and a session judge by eye — a report with an
        unopenable image is a report with no evidence in it."""
        spectrogram = Path(self.workspace.name) / "spectrogram.png"
        self.report_for(self.clean)
        self.assertEqual(spectrogram.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
        self.assertGreater(spectrogram.stat().st_size, 1024)


if __name__ == "__main__":
    unittest.main()
