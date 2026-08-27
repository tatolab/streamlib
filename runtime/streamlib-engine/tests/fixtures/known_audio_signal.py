# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Generate and analyse a known audio signal, with no StreamLib in the path.

The vivid property: this runs whether or not the engine compiles, so a failing
run tells you which side is broken. numpy is the only dependency, and the
spectrogram PNG is written by hand rather than through a plotting library.

The signal is a lead-in tone followed by DTMF digits. The tone answers "are
these samples audio at all" — frequency, amplitude, distortion. The digits
answer the question a tone cannot: a dropped or reordered block corrupts a
symbol, and the decode says which one.
"""

import json
import struct
import sys
import wave
import zlib

import numpy

SAMPLE_RATE = 48_000
LEAD_IN_SILENCE_SECONDS = 0.30
REFERENCE_TONE_HZ = 440.0
REFERENCE_TONE_SECONDS = 1.00
REFERENCE_AMPLITUDE = 0.5
DTMF_DIGIT_SECONDS = 0.12
DTMF_GAP_SECONDS = 0.08
TAIL_SILENCE_SECONDS = 0.20

# The fingerprint: what a run has to hand back to have carried the audio
# through intact.
DTMF_DIGITS = "482917"

DTMF_ROW_HZ = (697.0, 770.0, 852.0, 941.0)
DTMF_COLUMN_HZ = (1209.0, 1336.0, 1477.0, 1633.0)
DTMF_KEYPAD = ("123A", "456B", "789C", "*0#D")

# What a passing run has to land inside. The interval bound is the loosest of
# these deliberately: at 48 kHz one device quantum is ~10.7 ms, so 25 ms sits
# above the decoder's own 20 ms window jitter and below two lost quanta.
MAX_FUNDAMENTAL_ERROR_HZ = 1.0
MAX_AMPLITUDE_ERROR = 0.05
MAX_THD_PERCENT = 5.0
MAX_SYMBOL_INTERVAL_ERROR_MS = 25.0


# Long enough to kill the click at a segment edge, short enough that the tone's
# onset stays a sharp alignment landmark.
TONE_EDGE_RAMP_SECONDS = 0.003


def _tone(frequencies, seconds, amplitude):
    t = numpy.arange(int(seconds * SAMPLE_RATE)) / SAMPLE_RATE
    wave_form = sum(numpy.sin(2 * numpy.pi * hz * t) for hz in frequencies)
    shaped = amplitude * wave_form / len(frequencies)
    ramp_length = min(int(TONE_EDGE_RAMP_SECONDS * SAMPLE_RATE), len(shaped) // 2)
    if ramp_length:
        ramp = 0.5 - 0.5 * numpy.cos(numpy.linspace(0, numpy.pi, ramp_length))
        shaped[:ramp_length] *= ramp
        shaped[-ramp_length:] *= ramp[::-1]
    return shaped.astype("<f8")


def _silence(seconds):
    return numpy.zeros(int(seconds * SAMPLE_RATE), dtype="<f8")


def _dtmf_frequencies_for(digit):
    for row_index, row in enumerate(DTMF_KEYPAD):
        if digit in row:
            return (DTMF_ROW_HZ[row_index], DTMF_COLUMN_HZ[row.index(digit)])
    raise ValueError(f"{digit!r} is not a DTMF digit")


def generate_signal():
    segments = [
        _silence(LEAD_IN_SILENCE_SECONDS),
        _tone((REFERENCE_TONE_HZ,), REFERENCE_TONE_SECONDS, REFERENCE_AMPLITUDE),
        _silence(DTMF_GAP_SECONDS),
    ]
    for digit in DTMF_DIGITS:
        segments.append(
            _tone(_dtmf_frequencies_for(digit), DTMF_DIGIT_SECONDS, REFERENCE_AMPLITUDE)
        )
        segments.append(_silence(DTMF_GAP_SECONDS))
    segments.append(_silence(TAIL_SILENCE_SECONDS))
    return numpy.concatenate(segments)


def write_wav(path, samples):
    scaled = numpy.clip(samples, -1.0, 1.0)
    with wave.open(path, "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(SAMPLE_RATE)
        out.writeframes((scaled * 32767.0).astype("<i2").tobytes())


def read_wav(path):
    with wave.open(path, "rb") as source:
        channels = source.getnchannels()
        assert source.getsampwidth() == 2, "the fixture reads 16-bit PCM"
        frames = numpy.frombuffer(source.readframes(source.getnframes()), dtype="<i2")
        rate = source.getframerate()
    samples = frames.astype("<f8") / 32768.0
    if channels > 1:
        samples = samples.reshape(-1, channels).mean(axis=1)
    return samples, rate


def goertzel_magnitude(samples, frequency, rate):
    """One bin of a DFT, which is all a tone detector needs."""
    bin_index = int(0.5 + len(samples) * frequency / rate)
    omega = 2.0 * numpy.pi * bin_index / len(samples)
    coefficient = 2.0 * numpy.cos(omega)
    first = second = 0.0
    for sample in samples:
        first, second = sample + coefficient * first - second, first
    return numpy.sqrt(first * first + second * second - coefficient * first * second)


def first_sound_at(samples, rate, threshold=0.02):
    loud = numpy.flatnonzero(numpy.abs(samples) > threshold)
    return int(loud[0]) if loud.size else None


def measure_reference_tone(samples, rate):
    """Frequency, amplitude and distortion, from the middle of the tone."""
    window = samples[int(0.2 * rate) : int(0.8 * rate)]
    window = window * numpy.hanning(len(window))
    spectrum = numpy.abs(numpy.fft.rfft(window))
    frequencies = numpy.fft.rfftfreq(len(window), 1.0 / rate)
    fundamental_bin = int(numpy.argmax(spectrum[1:]) + 1)
    fundamental_hz = float(frequencies[fundamental_bin])

    def energy_near(hz):
        near = numpy.abs(frequencies - hz) < 15.0
        return float(numpy.sqrt(numpy.sum(spectrum[near] ** 2)))

    harmonics = numpy.sqrt(
        sum(energy_near(fundamental_hz * n) ** 2 for n in range(2, 6))
    )
    fundamental_energy = energy_near(fundamental_hz)
    raw = samples[int(0.2 * rate) : int(0.8 * rate)]
    return {
        "fundamental_hz": round(fundamental_hz, 2),
        "amplitude": round(float(numpy.sqrt(numpy.mean(raw**2)) * numpy.sqrt(2)), 4),
        "thd_percent": round(
            100.0 * float(harmonics) / max(fundamental_energy, 1e-12), 3
        ),
    }


def decode_dtmf(samples, rate):
    """Classify short windows, then collapse runs into digits.

    Each digit comes back with the instant it started, which is what turns the
    symbol stream into a timing grid: identity alone survives a dropped block
    (clipping 40 ms off a 120 ms digit still decodes to that digit), but every
    later digit shifts earlier by exactly the audio that went missing.
    """
    window_length = int(0.020 * rate)
    decoded = []
    previous = None
    for start in range(0, len(samples) - window_length, window_length):
        window = samples[start : start + window_length]
        if numpy.sqrt(numpy.mean(window**2)) < 0.02:
            previous = None
            continue
        rows = [goertzel_magnitude(window, hz, rate) for hz in DTMF_ROW_HZ]
        columns = [goertzel_magnitude(window, hz, rate) for hz in DTMF_COLUMN_HZ]
        row, column = int(numpy.argmax(rows)), int(numpy.argmax(columns))
        # Both a row and a column tone have to dominate their group, or this is
        # a single-frequency tone (the reference) rather than a digit.
        if max(rows) < 2.0 * numpy.median(rows) or max(columns) < 2.0 * numpy.median(
            columns
        ):
            previous = None
            continue
        digit = DTMF_KEYPAD[row][column]
        if digit != previous:
            decoded.append((digit, start / rate))
        previous = digit
    return decoded


def count_gaps(samples, rate, from_sample, to_sample, quiet_seconds=0.15):
    """Silent runs longer than the signal's own gaps — a hole in the stream."""
    region = numpy.abs(samples[from_sample:to_sample])
    quiet = region < 0.02
    gaps, run = 0, 0
    for is_quiet in quiet:
        run = run + 1 if is_quiet else 0
        if run == int(quiet_seconds * rate):
            gaps += 1
    return gaps


def write_spectrogram_png(path, samples, rate):
    """STFT magnitude as a PNG, written by hand so the fixture needs no plotter."""
    window_length, hop = 1024, 256
    columns = []
    for start in range(0, len(samples) - window_length, hop):
        block = samples[start : start + window_length] * numpy.hanning(window_length)
        columns.append(numpy.abs(numpy.fft.rfft(block))[: window_length // 4])
    if not columns:
        return
    magnitude = numpy.array(columns).T
    decibels = 20.0 * numpy.log10(magnitude + 1e-9)
    decibels = numpy.clip(decibels, decibels.max() - 70.0, decibels.max())
    normalised = (decibels - decibels.min()) / max(float(numpy.ptp(decibels)), 1e-9)
    image = numpy.flipud((normalised * 255).astype(numpy.uint8))
    # Blue → yellow, so a tone reads as a bright line on a dark field.
    rgb = numpy.dstack(
        [image, image, (255 - image * 0.7).astype(numpy.uint8)]
    ).astype(numpy.uint8)

    height, width, _ = rgb.shape
    raw = b"".join(b"\x00" + rgb[row].tobytes() for row in range(height))

    def chunk(kind, payload):
        return (
            struct.pack(">I", len(payload))
            + kind
            + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
        )

    with open(path, "wb") as out:
        out.write(b"\x89PNG\r\n\x1a\n")
        out.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)))
        out.write(chunk(b"IDAT", zlib.compress(raw, 6)))
        out.write(chunk(b"IEND", b""))


def analyse(captured_path, spectrogram_path):
    samples, rate = read_wav(captured_path)
    onset = first_sound_at(samples, rate)
    if onset is None:
        return {"verdict": "FAIL", "reason": "the capture is silent end to end"}

    aligned = samples[onset:]
    tone_metrics = measure_reference_tone(aligned, rate)
    dtmf_region_start = int((REFERENCE_TONE_SECONDS + DTMF_GAP_SECONDS / 2) * rate)
    decoded = decode_dtmf(aligned[dtmf_region_start:], rate)
    write_spectrogram_png(spectrogram_path, aligned, rate)

    # The interval between consecutive digits, not their absolute positions:
    # audio that went missing between two digits shortens exactly that one
    # interval, which both detects the loss and says where it happened —
    # while a constant alignment offset cancels out.
    digit_period = DTMF_DIGIT_SECONDS + DTMF_GAP_SECONDS
    timing_error_ms = None
    worst_interval = None
    if len(decoded) == len(DTMF_DIGITS):
        intervals = [
            (decoded[i + 1][1] - decoded[i][1], i) for i in range(len(decoded) - 1)
        ]
        worst, at = max(intervals, key=lambda pair: abs(pair[0] - digit_period))
        timing_error_ms = round(1000.0 * (worst - digit_period), 1)
        worst_interval = f"{DTMF_DIGITS[at]}->{DTMF_DIGITS[at + 1]}"

    expected_dtmf_end = dtmf_region_start + int(
        len(DTMF_DIGITS) * (DTMF_DIGIT_SECONDS + DTMF_GAP_SECONDS) * rate
    )
    report = {
        **tone_metrics,
        "symbols": "".join(digit for digit, _ in decoded),
        "symbols_expected": DTMF_DIGITS,
        "symbol_interval_error_ms": timing_error_ms,
        "worst_symbol_interval": worst_interval,
        "gap_count": count_gaps(aligned, rate, 0, expected_dtmf_end),
        "captured_seconds": round(len(samples) / rate, 3),
        "onset_seconds": round(onset / rate, 3),
    }
    failures = []
    if abs(report["fundamental_hz"] - REFERENCE_TONE_HZ) >= MAX_FUNDAMENTAL_ERROR_HZ:
        failures.append("fundamental_hz")
    if abs(report["amplitude"] - REFERENCE_AMPLITUDE) >= MAX_AMPLITUDE_ERROR:
        failures.append("amplitude")
    if report["thd_percent"] >= MAX_THD_PERCENT:
        failures.append("thd_percent")
    if report["symbols"] != DTMF_DIGITS:
        failures.append("symbols")
    if report["gap_count"] != 0:
        failures.append("gap_count")
    if timing_error_ms is None or abs(timing_error_ms) > MAX_SYMBOL_INTERVAL_ERROR_MS:
        failures.append("symbol_interval_error_ms")
    report["failed"] = failures
    report["verdict"] = "PASS" if not failures else "FAIL"
    return report


def main(argv):
    if len(argv) >= 3 and argv[1] == "generate":
        write_wav(argv[2], generate_signal())
        return 0
    if len(argv) >= 4 and argv[1] == "analyse":
        report = analyse(argv[2], argv[3])
        print(json.dumps(report, indent=2))
        # The exit status IS the verdict, so a caller gates on it without
        # parsing anything.
        return 0 if report["verdict"] == "PASS" else 1
    print(
        "Usage: known_audio_signal.py generate <signal.wav>\n"
        "       known_audio_signal.py analyse <captured.wav> <spectrogram.png>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
