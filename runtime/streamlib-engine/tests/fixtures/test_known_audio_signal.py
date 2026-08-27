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

    def signal_silenced(self, milliseconds, at_seconds):
        """An underrun: the samples are replaced, not removed."""
        silenced = self.clean.copy()
        at = int(at_seconds * fixture.SAMPLE_RATE)
        silenced[at : at + int(milliseconds / 1000.0 * fixture.SAMPLE_RATE)] = 0.0
        return silenced

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
        """Identity survives the loss and the fundamental does not move; the
        span between two onsets shortens by exactly what went missing.

        Not single-axis, and correctly so: samples really did leave, so the
        signal really is short and the digit really does have a hole where its
        body should be. What matters is that the span is named, because that is
        the part a reader acts on.
        """
        report = self.report_for(self.signal_missing(20.0, at_seconds=1.85))

        self.assertIn("symbol_interval_error_ms", report["failed"], report)
        self.assertNotIn("fundamental_hz", report["failed"])
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

    def test_an_underrun_filled_with_silence_is_caught(self):
        """The shape a real device produces, and the one the span check cannot
        see: an xrun does not drop samples, it substitutes silence, so nothing
        shortens and every landmark stays exactly where it was."""
        for at_seconds, region in ((0.70, "the tone"), (1.80, "a digit")):
            with self.subTest(region=region):
                report = self.report_for(
                    self.signal_silenced(A_DEVICE_QUANTUM_MS, at_seconds=at_seconds)
                )
                # Both silence axes, and only those: a hole inside a body is a
                # contiguous quiet run AND sound the body should have carried.
                self.assertEqual(
                    report["failed"],
                    ["silent_stretch_ms", "missing_loud_audio_ms"],
                    report,
                )
                self.assertAlmostEqual(
                    report["silent_stretch_ms"], A_DEVICE_QUANTUM_MS, delta=1.0
                )

    def test_a_capture_at_the_wrong_sample_rate_is_refused(self):
        """Every other measurement normalises by the file's own header rate, so
        a path that genuinely resampled cancels out of all of them — an 8 kHz
        capture of this signal reads as perfectly healthy until the rate itself
        is checked."""
        captured = Path(self.workspace.name) / "resampled.wav"
        fixture.write_wav(str(captured), self.clean, rate=44_100)
        report = fixture.analyse(
            str(captured), str(Path(self.workspace.name) / "spectrogram.png")
        )
        self.assertIn("captured_sample_rate", report["failed"], report)
        self.assertEqual(report["captured_sample_rate"], 44_100)

    def test_an_underrun_straddling_a_symbol_edge_is_caught(self):
        """The longest contiguous quiet run only sees the part of a hole that
        landed inside a body, so a hole on an edge under-reports. Total sound
        missing from the body catches it wherever it falls."""
        digit_starts_at = (
            fixture.LEAD_IN_SILENCE_SECONDS
            + fixture.REFERENCE_TONE_SECONDS
            + fixture.DTMF_GAP_SECONDS
        )
        report = self.report_for(
            self.signal_silenced(A_DEVICE_QUANTUM_MS, at_seconds=digit_starts_at - 0.004)
        )
        self.assertIn("missing_loud_audio_ms", report["failed"], report)
        self.assertEqual(report["emptiest_region"], fixture.DTMF_DIGITS[0])

    def test_a_capture_that_stops_inside_the_last_symbol_is_caught(self):
        """The landmark grid ends at the last onset, so the last symbol's own
        body needs a bound of its own — and a recorder that keeps writing
        silence leaves the file the right length while the audio is gone."""
        stops_at = int(2.49 * fixture.SAMPLE_RATE)
        still_recording = numpy.concatenate(
            [self.clean[:stops_at], numpy.zeros(int(1.2 * fixture.SAMPLE_RATE))]
        )
        self.assertIn(
            "signal_ended_early", self.report_for(still_recording)["failed"]
        )
        self.assertIn(
            "signal_ended_early", self.report_for(self.clean[:stops_at])["failed"]
        )

    def test_loss_spread_across_every_span_still_adds_up(self):
        """Repeated small xruns stay under the per-span bound while the total
        does not, so the grid is checked against its own length as well."""
        thinned = self.clean
        for at_seconds in (0.9, 1.15, 1.35, 1.55, 1.75, 1.95, 2.15):
            cut = int(at_seconds * fixture.SAMPLE_RATE)
            lost = int(4.99 / 1000.0 * fixture.SAMPLE_RATE)
            thinned = numpy.concatenate([thinned[:cut], thinned[cut + lost :]])
        report = self.report_for(thinned)
        self.assertIn("cumulative_interval_error_ms", report["failed"], report)

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
