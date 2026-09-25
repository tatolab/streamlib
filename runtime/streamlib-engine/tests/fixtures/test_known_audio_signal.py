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

import contextlib
import io
import json
import subprocess
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
        demo app: it has to run and report when StreamLib will not build.

        Asked in its own interpreter, because `sys.modules` is process-wide and
        any sibling suite that legitimately imports the wheel would otherwise
        answer this question on the analyser's behalf.
        """
        proof = subprocess.run(
            [
                sys.executable,
                "-c",
                "import sys, known_audio_signal;"
                " sys.exit(1 if 'streamlib' in sys.modules else 0)",
            ],
            cwd=str(Path(__file__).parent),
            capture_output=True,
        )
        self.assertEqual(
            proof.returncode, 0, "importing the analyser pulled in the engine"
        )

    def test_the_spectrogram_is_a_readable_png(self):
        """The half a human and a session judge by eye — a report with an
        unopenable image is a report with no evidence in it."""
        spectrogram = Path(self.workspace.name) / "spectrogram.png"
        self.report_for(self.clean)
        self.assertEqual(spectrogram.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
        self.assertGreater(spectrogram.stat().st_size, 1024)

    def test_the_digital_report_carries_nothing_of_the_acoustic_path(self):
        report = self.report_for(self.clean)
        for acoustic_only in ("capture_path", "tone_to_noise_db", "exact_zero_stretch_ms"):
            self.assertNotIn(acoustic_only, report)


def heard_through_a_room(
    played,
    gain_db=-30.0,
    room_noise_dbfs=-75.0,
    reverberation_time_seconds=0.3,
    direct_to_reverberant_db=10.0,
    speaker_corner_hz=600.0,
    latency_seconds=0.025,
):
    """What a laptop's microphone hears when its speaker plays `played`.

    Every way the air differs from a wire, each stated: the volume knob's gain,
    a small driver's bass roll-off, a reverberant tail, the trip's latency and
    the room's own noise. Seeded, so a run is repeatable.
    """
    noise = numpy.random.default_rng(2411)
    rate = fixture.SAMPLE_RATE
    frequencies = numpy.fft.rfftfreq(len(played), 1.0 / rate)
    speaker_response = frequencies / numpy.sqrt(frequencies**2 + speaker_corner_hz**2)
    coloured = numpy.fft.irfft(numpy.fft.rfft(played) * speaker_response, n=len(played))

    tail_length = int(reverberation_time_seconds * rate)
    tail_time = numpy.arange(tail_length) / rate
    # 60 dB down at the reverberation time.
    tail = noise.normal(0.0, 1.0, tail_length) * numpy.exp(
        -6.908 * tail_time / reverberation_time_seconds
    )
    tail *= 10.0 ** (-direct_to_reverberant_db / 20.0) / numpy.sqrt(numpy.sum(tail**2))
    room_response = numpy.concatenate([[1.0], tail])
    length = len(coloured) + len(room_response)
    reverberant = numpy.fft.irfft(
        numpy.fft.rfft(coloured, length) * numpy.fft.rfft(room_response, length), length
    )[: len(coloured) + tail_length]

    heard = numpy.concatenate(
        [numpy.zeros(int(latency_seconds * rate)), reverberant, numpy.zeros(rate // 2)]
    ) * 10.0 ** (gain_db / 20.0)
    return heard + noise.normal(0.0, 10.0 ** (room_noise_dbfs / 20.0), len(heard))


class KnownAudioSignalThroughTheAir(unittest.TestCase):
    """The acoustic path: normalised first, then held to what the air leaves.

    Each control passes the signal through a synthetic room, so what is proven
    is that the relaxed parameter set still goes red on the losses it claims to
    see — and says plainly which ones it no longer can.
    """

    def setUp(self):
        self.workspace = tempfile.TemporaryDirectory()
        self.addCleanup(self.workspace.cleanup)
        self.clean = fixture.generate_signal()
        self.heard = heard_through_a_room(self.clean)

    def report_for(self, samples, capture_path=fixture.ACOUSTIC_CAPTURE_PATH):
        captured = Path(self.workspace.name) / "captured.wav"
        fixture.write_wav(str(captured), samples)
        return fixture.analyse(
            str(captured), str(Path(self.workspace.name) / "spectrogram.png"), capture_path
        )

    def heard_with_a_capture_hole(self, milliseconds, at_seconds):
        """A capture-side underrun: the recorder places blocks by their stamps,
        so audio the capture never delivered is exact zeros."""
        holed = self.heard.copy()
        at = int(at_seconds * fixture.SAMPLE_RATE)
        holed[at : at + int(milliseconds / 1000.0 * fixture.SAMPLE_RATE)] = 0.0
        return holed

    def test_a_capture_through_the_air_passes(self):
        report = self.report_for(self.heard)
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertEqual(report["symbols"], fixture.DTMF_DIGITS)
        self.assertEqual(report["capture_path"], "acoustic")

    def test_a_capture_the_acoustic_path_passes_fails_the_digital_path(self):
        """Why the acoustic path exists: the level is the volume knob's, so
        even a loud trip through the air misses the digital amplitude bound."""
        heard_loudly = heard_through_a_room(self.clean, gain_db=-6.0)
        self.assertEqual(self.report_for(heard_loudly)["verdict"], "PASS")
        report = self.report_for(heard_loudly, fixture.DIGITAL_CAPTURE_PATH)
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("amplitude", report["failed"])

    def test_a_reverberant_room_still_decodes(self):
        for room in (
            {"reverberation_time_seconds": 0.6},
            {"direct_to_reverberant_db": 3.0},
        ):
            with self.subTest(**room):
                report = self.report_for(heard_through_a_room(self.clean, **room))
                self.assertEqual(report["verdict"], "PASS", report)

    def test_a_capture_hole_is_still_caught(self):
        """The air never delivers exact zeros, so a hole the capture side left
        stays visible however much the room colours everything else."""
        tone_body = fixture.LEAD_IN_SILENCE_SECONDS + 0.025 + 0.7
        digit_body = (
            fixture.LEAD_IN_SILENCE_SECONDS
            + 0.025
            + fixture.REFERENCE_TONE_SECONDS
            + fixture.DTMF_GAP_SECONDS
            + fixture.DTMF_DIGIT_SECONDS
            + fixture.DTMF_GAP_SECONDS
            + 0.05
        )
        for at_seconds, region in ((tone_body, "the tone"), (digit_body, "a digit")):
            with self.subTest(region=region):
                report = self.report_for(
                    self.heard_with_a_capture_hole(A_DEVICE_QUANTUM_MS, at_seconds)
                )
                self.assertEqual(report["failed"], ["exact_zero_stretch_ms"], report)
                self.assertAlmostEqual(
                    report["exact_zero_stretch_ms"], A_DEVICE_QUANTUM_MS, delta=1.0
                )

    def test_a_dropped_capture_block_moves_the_spacing_and_names_the_span(self):
        cut = int(1.9 * fixture.SAMPLE_RATE)
        lost = int(A_DEVICE_QUANTUM_MS / 1000.0 * fixture.SAMPLE_RATE)
        report = self.report_for(
            numpy.concatenate([self.heard[:cut], self.heard[cut + lost :]])
        )
        self.assertIn("symbol_interval_error_ms", report["failed"], report)
        self.assertEqual(report["worst_symbol_interval"], "2->9")

    def test_a_block_dropped_before_the_speaker_moves_the_spacing_too(self):
        """The injected `drop` fault, played and heard: the one playback-side
        loss the room cannot refill, because it shortens the signal."""
        report = self.report_for(
            heard_through_a_room(fixture.signal_with_injected_fault(self.clean, "drop"))
        )
        self.assertIn("symbol_interval_error_ms", report["failed"], report)

    def test_a_room_too_loud_for_the_tone_fails_by_name(self):
        report = self.report_for(heard_through_a_room(self.clean, room_noise_dbfs=-60.0))
        self.assertEqual(report["failed"], ["tone_to_noise_db"], report)
        self.assertLess(report["tone_to_noise_db"], fixture.ACOUSTIC_MIN_TONE_TO_NOISE_DB)

    def test_a_sample_rate_mismatch_fails_on_frequency_and_symbols(self):
        misread = numpy.interp(
            numpy.arange(0, len(self.heard), 44_100 / 48_000),
            numpy.arange(len(self.heard)),
            self.heard,
        )
        report = self.report_for(misread)
        self.assertIn("fundamental_hz", report["failed"], report)
        self.assertIn("symbols", report["failed"])

    def test_a_played_gain_error_is_the_volume_knobs_and_passes(self):
        """The acoustic path cannot see the injected `gain` fault, and says so
        rather than pretending: amplitude is reported, never judged."""
        report = self.report_for(
            heard_through_a_room(fixture.signal_with_injected_fault(self.clean, "gain"))
        )
        self.assertEqual(report["verdict"], "PASS", report)
        self.assertIn("amplitude", report["reported_only"])

    def test_the_report_names_what_it_only_reports_and_what_is_uncalibrated(self):
        report = self.report_for(self.heard)
        self.assertEqual(
            report["reported_only"],
            ["amplitude", "thd_percent", "silent_stretch_ms", "missing_loud_audio_ms"],
        )
        self.assertEqual(
            report["thresholds_to_calibrate_from_attended_runs"],
            ["symbol_interval_error_ms", "cumulative_interval_error_ms"],
        )
        # The raw numbers a reader calibrates from: what the room delivered.
        self.assertAlmostEqual(report["room_noise_rms_dbfs"], -75.0, delta=1.5)
        self.assertLess(report["raw_tone_rms_dbfs"], -30.0)
        self.assertGreater(report["tone_to_noise_db"], fixture.ACOUSTIC_MIN_TONE_TO_NOISE_DB)

    def test_silence_is_refused_rather_than_measured(self):
        report = self.report_for(numpy.zeros_like(self.heard))
        self.assertEqual(report["verdict"], "FAIL", report)
        self.assertIn("silent", report["reason"])


class KnownAudioSignalCommandLine(unittest.TestCase):
    def setUp(self):
        workspace = tempfile.TemporaryDirectory()
        self.addCleanup(workspace.cleanup)
        self.workspace = Path(workspace.name)

    def wav_of(self, samples):
        path = self.workspace / "captured.wav"
        fixture.write_wav(str(path), samples)
        return str(path)

    def test_analyse_takes_the_acoustic_path_by_name(self):
        heard = self.wav_of(heard_through_a_room(fixture.generate_signal()))
        spectrogram = str(self.workspace / "spectrogram.png")
        with contextlib.redirect_stdout(io.StringIO()) as printed:
            status = fixture.main(["", "analyse", heard, spectrogram, "--path", "acoustic"])
        self.assertEqual(status, 0, printed.getvalue())
        self.assertEqual(json.loads(printed.getvalue())["capture_path"], "acoustic")

    def test_an_unknown_path_is_a_usage_error(self):
        heard = self.wav_of(fixture.generate_signal())
        with contextlib.redirect_stderr(io.StringIO()):
            status = fixture.main(
                ["", "analyse", heard, str(self.workspace / "s.png"), "--path", "wired"]
            )
        self.assertEqual(status, 2)

    def test_exact_digital_silence_is_told_from_anything_else(self):
        """What a Core Audio tap delivers without its grant, told from a capture
        that is merely quiet."""
        self.assertEqual(
            fixture.main(["", "exact-digital-silence", self.wav_of(numpy.zeros(4800))]), 0
        )
        barely_audible = numpy.zeros(4800)
        barely_audible[100] = 2.0 / 32768.0
        self.assertEqual(
            fixture.main(["", "exact-digital-silence", self.wav_of(barely_audible)]), 1
        )


if __name__ == "__main__":
    unittest.main()
