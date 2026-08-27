# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Generate and analyse a known audio signal, with no StreamLib in the path.

The vivid property: this runs whether or not the engine compiles, so a failing
run tells you which side is broken. numpy is the only dependency, and the
spectrogram PNG is written by hand rather than through a plotting library.

The signal is a lead-in tone followed by DTMF digits. The tone answers "are
these samples audio at all" — frequency, amplitude, distortion. The digits are
a timing grid, and the grid is the point: a symbol's *identity* survives a
partial loss exactly as the tone's frequency does, because clipping part of a
digit still decodes as that digit. What moves is the interval between one
onset and the next, and it moves by precisely the audio that went missing —
which both detects the loss and says where in the signal it happened.

What is guarded is the tone body and each symbol body — roughly five sixths of
the signal. The gaps between symbols are silent by construction, so an underrun
inside one substitutes silence for silence and no measurement can see it.
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

# What a passing run has to land inside.
MAX_FUNDAMENTAL_ERROR_HZ = 1.0
MAX_AMPLITUDE_ERROR = 0.05
MAX_THD_PERCENT = 5.0
# One device quantum at 48 kHz is ~10.7 ms, and a single dropped block is the
# loss this whole signal design exists to catch — so the bound sits below one
# quantum rather than above it. Onsets are found to ~1 ms, so what this really
# bounds is the capture path's own jitter.
MAX_SYMBOL_INTERVAL_ERROR_MS = 5.0

# Onset search: step, the window RMS is taken over, and how much quiet has to
# precede an edge for it to be a symbol starting rather than noise.
# An underrun does not shorten the stream: the device fills it with silence and
# every timestamp after it stays put, so no span moves and the loss is
# invisible to the landmark check. These bound the two things that DO change —
# a hole where the signal is known to be loud, and a capture that stops early.
MAX_SILENT_STRETCH_MS = 6.0
# Loss spread thinly across every span stays under the per-span bound while
# still adding up, so the whole grid is checked against its own total.
MAX_CUMULATIVE_INTERVAL_ERROR_MS = 10.0

ONSET_SEARCH_HOP_SECONDS = 0.001
ONSET_ENERGY_WINDOW_SECONDS = 0.005
QUIET_BEFORE_A_SYMBOL_SECONDS = 0.030
# Sound has to persist this long to be the signal starting rather than a click.
MIN_SUSTAINED_SOUND_SECONDS = 0.020
SYMBOL_CLASSIFY_WINDOW_SECONDS = 0.060
SOUND_THRESHOLD = 0.02

# The reference tone's own onset is the first landmark, so the span from it to
# the first digit is guarded like every span between digits. Named because the
# report uses it to say where a loss happened.
REFERENCE_TONE_LANDMARK = "tone"


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


def signal_with_injected_fault(samples, fault):
    """A deliberately broken signal, so a live run can be seen going red.

    The sibling PSNR fixtures each carry one of these for the same reason: a
    gate that has only ever been observed green is a gate nobody has evidence
    is still wired up.
    """
    broken = samples.copy()
    # Inside the reference tone, which is loud by construction. An inter-symbol
    # gap is silent by design, so substituting silence there loses nothing and
    # nothing can detect it.
    at = int((LEAD_IN_SILENCE_SECONDS + 0.4) * SAMPLE_RATE)
    if fault == "silence":
        broken[at : at + int(0.03 * SAMPLE_RATE)] = 0.0
        return broken
    if fault == "drop":
        return numpy.concatenate([broken[:at], broken[at + int(0.03 * SAMPLE_RATE) :]])
    if fault == "gain":
        return broken * 0.6
    raise ValueError(f"{fault!r} is not an injectable fault")


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


def first_sound_at(samples, rate, threshold=SOUND_THRESHOLD):
    """The instant the signal starts, from sustained energy rather than one sample.

    A single threshold crossing is whatever the capture path clicked on first:
    one stray pop ahead of the tone would anchor every downstream window to the
    wrong origin and produce a confident failure on axes unrelated to the cause.
    """
    energy, hop = short_time_rms(samples, rate)
    loud = energy > threshold
    sustain = max(1, int(MIN_SUSTAINED_SOUND_SECONDS / ONSET_SEARCH_HOP_SECONDS))
    for frame in range(max(0, len(loud) - sustain + 1)):
        if loud[frame : frame + sustain].all():
            return frame * hop
    return None


def last_sound_at(samples, rate, threshold=SOUND_THRESHOLD):
    """The instant the signal stops, found the way its onset is.

    The landmark grid ends at the last symbol's onset, so nothing in it can see
    a capture that stopped inside that symbol — and a recorder that keeps
    writing silence leaves the file the right length while the audio is gone.
    """
    energy, hop = short_time_rms(samples, rate)
    loud = energy > threshold
    sustain = max(1, int(MIN_SUSTAINED_SOUND_SECONDS / ONSET_SEARCH_HOP_SECONDS))
    for frame in range(len(loud) - sustain, -1, -1):
        if loud[frame : frame + sustain].all():
            return (frame + sustain) * hop
    return None


def short_time_rms(samples, rate):
    """RMS on a fine grid — the resolution every onset in the report inherits."""
    hop = max(1, int(ONSET_SEARCH_HOP_SECONDS * rate))
    window = max(1, int(ONSET_ENERGY_WINDOW_SECONDS * rate))
    frame_count = max(0, (len(samples) - window) // hop + 1)
    energy = numpy.array(
        [
            numpy.sqrt(numpy.mean(samples[frame * hop : frame * hop + window] ** 2))
            for frame in range(frame_count)
        ]
    )
    return energy, hop


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
    tone_samples = samples[int(0.2 * rate) : int(0.8 * rate)]
    return {
        "fundamental_hz": round(fundamental_hz, 2),
        "amplitude": round(
            float(numpy.sqrt(numpy.mean(tone_samples**2)) * numpy.sqrt(2)), 4
        ),
        "thd_percent": round(
            100.0 * float(harmonics) / max(fundamental_energy, 1e-12), 3
        ),
    }


def classify_dtmf(window, rate):
    """The digit a window carries, or None if it is not two tones at once.

    The reference tone rejects itself here: it is one frequency, so neither
    group has a dominant member and the tone never enters the grid as a digit.
    """
    rows = [goertzel_magnitude(window, hz, rate) for hz in DTMF_ROW_HZ]
    columns = [goertzel_magnitude(window, hz, rate) for hz in DTMF_COLUMN_HZ]
    if max(rows) < 2.0 * numpy.median(rows) or max(columns) < 2.0 * numpy.median(
        columns
    ):
        return None
    return DTMF_KEYPAD[int(numpy.argmax(rows))][int(numpy.argmax(columns))]


def decode_dtmf(samples, rate):
    """Every digit in the signal, each with the instant it started.

    The edge is found first and the digit classified afterwards, rather than
    classifying on a fixed window grid: a grid quantises every onset to its own
    width, which would put the smallest detectable loss at several device
    quanta instead of below one.
    """
    energy, hop = short_time_rms(samples, rate)
    loud = energy > SOUND_THRESHOLD
    quiet_frames = max(1, int(QUIET_BEFORE_A_SYMBOL_SECONDS / ONSET_SEARCH_HOP_SECONDS))
    classify_length = int(SYMBOL_CLASSIFY_WINDOW_SECONDS * rate)

    decoded = []
    frame = quiet_frames
    while frame < len(loud):
        if not (loud[frame] and not loud[frame - quiet_frames : frame].any()):
            frame += 1
            continue
        onset = frame * hop
        digit = classify_dtmf(samples[onset : onset + classify_length], rate)
        if digit is not None:
            decoded.append((digit, onset / rate))
        # Past the symbol's own body, so it cannot read as a second edge.
        frame += quiet_frames
    return decoded


def longest_silence_where_the_signal_is_loud(samples, rate, decoded):
    """The longest quiet run inside a region the signal says carries sound.

    This is the axis an underrun trips. A device that xruns fills the hole with
    silence rather than dropping samples, so nothing shortens, every landmark
    stays where it was and the span check sees a clean run — but the tone body
    and each digit body are known-loud by construction, and a hole in one of
    them is not.
    """
    # Raw samples rather than the windowed RMS every other measurement uses: a
    # window smears a hole across its own width, so a one-quantum hole would
    # read several milliseconds shorter than it is and slip under the bound. A
    # sine's zero crossings and a two-tone beat null are both far shorter than
    # any threshold worth setting here, so the raw signal is safe to read.
    margin = 2 * TONE_EDGE_RAMP_SECONDS
    bodies = [(margin, REFERENCE_TONE_SECONDS - margin)]
    bodies += [
        (onset + margin, onset + DTMF_DIGIT_SECONDS - margin) for _, onset in decoded
    ]

    longest_run = 0
    for body_start, body_end in bodies:
        body = numpy.abs(samples[int(body_start * rate) : int(body_end * rate)])
        quiet = numpy.concatenate(([False], body <= SOUND_THRESHOLD, [False]))
        edges = numpy.flatnonzero(quiet[1:] != quiet[:-1])
        if edges.size:
            longest_run = max(longest_run, int((edges[1::2] - edges[::2]).max()))
    return round(1000.0 * longest_run / rate, 1)


def seconds_the_signal_occupies_after_its_onset():
    """Where the last digit ends, measured from the tone's onset.

    A capture that stops before this lost audio no landmark can miss it for:
    the span check ends at the last onset, so the last symbol's own body and
    everything after it needs a bound of its own.
    """
    return (
        REFERENCE_TONE_SECONDS
        + DTMF_GAP_SECONDS
        + (len(DTMF_DIGITS) - 1) * (DTMF_DIGIT_SECONDS + DTMF_GAP_SECONDS)
        + DTMF_DIGIT_SECONDS
    )


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
    png_scanline_bytes = b"".join(b"\x00" + rgb[row].tobytes() for row in range(height))

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
        out.write(chunk(b"IDAT", zlib.compress(png_scanline_bytes, 6)))
        out.write(chunk(b"IEND", b""))


def worst_landmark_interval(decoded):
    """The span that deviates most from what the signal was built with.

    Every landmark is included, starting with the reference tone's own onset,
    so the whole signal from the tone to the last symbol sits between two
    landmarks. Measuring spans rather than absolute positions means a constant
    alignment offset cancels while a real loss does not; audio that goes
    missing inside a span shortens exactly that one, and the span is named.
    """
    landmarks = [(REFERENCE_TONE_LANDMARK, 0.0), *decoded]
    if len(decoded) != len(DTMF_DIGITS):
        return None, None, None
    digit_period = DTMF_DIGIT_SECONDS + DTMF_GAP_SECONDS
    expected = [REFERENCE_TONE_SECONDS + DTMF_GAP_SECONDS] + [digit_period] * (
        len(decoded) - 1
    )
    deviations = [
        (landmarks[i + 1][1] - landmarks[i][1] - expected[i], i)
        for i in range(len(expected))
    ]
    deviation, span_start = max(deviations, key=lambda pair: abs(pair[0]))
    cumulative = landmarks[-1][1] - landmarks[0][1] - sum(expected)
    return (
        round(1000.0 * deviation, 1),
        f"{landmarks[span_start][0]}->{landmarks[span_start + 1][0]}",
        round(1000.0 * cumulative, 1),
    )


def analyse(captured_path, spectrogram_path):
    samples, rate = read_wav(captured_path)
    onset = first_sound_at(samples, rate)
    if onset is None:
        return {"verdict": "FAIL", "reason": "the capture is silent end to end"}

    aligned = samples[onset:]
    tone_metrics = measure_reference_tone(aligned, rate)
    decoded = decode_dtmf(aligned, rate)
    write_spectrogram_png(spectrogram_path, aligned, rate)

    timing_error_ms, worst_interval, cumulative_error_ms = worst_landmark_interval(
        decoded
    )
    report = {
        **tone_metrics,
        "symbols": "".join(digit for digit, _ in decoded),
        "symbols_expected": DTMF_DIGITS,
        "symbol_interval_error_ms": timing_error_ms,
        "worst_symbol_interval": worst_interval,
        "cumulative_interval_error_ms": cumulative_error_ms,
        "silent_stretch_ms": longest_silence_where_the_signal_is_loud(
            aligned, rate, decoded
        ),
        "captured_after_onset_seconds": round(len(aligned) / rate, 3),
        "sound_ends_at_seconds": round((last_sound_at(aligned, rate) or 0) / rate, 3),
        "signal_expected_seconds": round(seconds_the_signal_occupies_after_its_onset(), 3),
        "captured_seconds": round(len(samples) / rate, 3),
        "captured_sample_rate": rate,
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
    if report["silent_stretch_ms"] > MAX_SILENT_STRETCH_MS:
        failures.append("silent_stretch_ms")
    if (
        report["sound_ends_at_seconds"]
        < report["signal_expected_seconds"] - MAX_SILENT_STRETCH_MS / 1000.0
    ):
        failures.append("signal_ended_early")
    if timing_error_ms is None or abs(timing_error_ms) > MAX_SYMBOL_INTERVAL_ERROR_MS:
        failures.append("symbol_interval_error_ms")
    if (
        cumulative_error_ms is None
        or abs(cumulative_error_ms) > MAX_CUMULATIVE_INTERVAL_ERROR_MS
    ):
        failures.append("cumulative_interval_error_ms")
    report["failed"] = failures
    report["verdict"] = "PASS" if not failures else "FAIL"
    return report


def main(argv):
    if len(argv) >= 3 and argv[1] == "generate":
        signal = generate_signal()
        if len(argv) >= 5 and argv[3] == "--inject":
            signal = signal_with_injected_fault(signal, argv[4])
        write_wav(argv[2], signal)
        return 0
    if len(argv) >= 4 and argv[1] == "analyse":
        report = analyse(argv[2], argv[3])
        print(json.dumps(report, indent=2))
        # The exit status IS the verdict, so a caller gates on it without
        # parsing anything.
        return 0 if report["verdict"] == "PASS" else 1
    print(
        "Usage: known_audio_signal.py generate <signal.wav> "
        "[--inject silence|drop|gain]\n"
        "       known_audio_signal.py analyse <captured.wav> <spectrogram.png>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
