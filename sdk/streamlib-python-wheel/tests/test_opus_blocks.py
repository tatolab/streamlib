# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Opus codec pair, marker class to decoded audio block.

The marker tests are pure Python. The graph tests boot a real engine, so they
carry `requires_gpu` like every other graph test here and run nowhere in CI —
libopus needs no device, but `Runtime` needs a GPU context.

No microphone: a Python source publishes a stereo tone at a stated rate, so
the channel count the encoder follows and the rate the decoder reconstructs at
are the test's own facts rather than the machine's.

The encoded-channel test is where the cast meets the engine: what
`EncodedAudioPacket` says an encoded bag is, asserted against bags libopus
actually wrote. Its GPU-free half — the wire keys, the refusals, the payload's
msgpack type — is `test_encoded_audio_packet_cast.py`.
"""

import json
import re
from pathlib import Path

import pytest

import streamlib
from streamlib import OpusDecoder, OpusEncoder
from opus_blocks_probes import (
    DECODED_BLOCKS_REPORTED,
    ENCODED_PACKETS_REPORTED,
    SOURCE_CHANNELS,
    SOURCE_SAMPLE_RATE,
)

OPUS_BLOCKS_APP = Path(__file__).parent / "opus_blocks_app.py"

ENCODED_PACKET = re.compile(r"MARKER:ENCODED_PACKET (\{.*\})")
DECODED_BLOCK = re.compile(r"MARKER:DECODED_BLOCK (\{.*\})")

TWO_OPUS_MARKERS = [OpusEncoder, OpusDecoder]

# The framing the encoder's own window contract fixes: 20 ms at Opus's 48 kHz
# clock. Every packet spans exactly this many per-channel samples.
SAMPLES_IN_ONE_OPUS_PACKET = 960
NANOSECONDS_PER_OPUS_PACKET = SAMPLES_IN_ONE_OPUS_PACKET * 1_000_000_000 // 48_000

# libopus's lookahead at 48 kHz is `Fs/400 + Fs/250` = 312 samples, and 120 at
# `lowdelay`. The assertions read `pre_skip` off the bag rather than assuming
# either — what is under test is that the decoder trims exactly what the
# encoder reported, not what this file believes libopus reports.
PRE_SKIP_SAMPLES_A_CREDIBLE_ENCODER_REPORTS = range(1, SAMPLES_IN_ONE_OPUS_PACKET)


# ---- marker semantics (no GPU) ---------------------------------------------


@pytest.mark.parametrize("marker_class", TWO_OPUS_MARKERS)
def test_the_marker_class_cannot_be_instantiated(marker_class):
    with pytest.raises(TypeError):
        marker_class()


@pytest.mark.parametrize("marker_class", TWO_OPUS_MARKERS)
def test_display_name_defaults_to_the_type_name(marker_class):
    runtime = streamlib.Runtime()
    try:
        block = runtime.add(marker_class)
        assert block.display_name == marker_class.__name__
    finally:
        runtime.shutdown()


def test_the_round_trip_wires_without_an_adapter():
    """Source into encoder, encoder into decoder — the port names compose as
    published, which is what makes three `rt.add` calls and two `rt.connect`
    calls the whole of an audio codec round trip. No rechunker between the
    source and the encoder: the encoder's own window contract frames."""
    runtime = streamlib.Runtime()
    try:
        microphone = runtime.add(streamlib.MicrophoneSource)
        encoder = runtime.add(OpusEncoder)
        decoder = runtime.add(OpusDecoder)
        speaker = runtime.add(streamlib.SpeakerSink)
        runtime.connect(microphone.output("audio"), encoder.input("audio"))
        runtime.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
        runtime.connect(decoder.output("audio"), speaker.input("audio"))
    finally:
        runtime.shutdown()


# ---- the round trip in a real graph (GPU) ----------------------------------


def _reported(pattern: "re.Pattern[str]", app_output: str) -> "list[dict]":
    """Every report the probe admitted, in the order it admitted them."""
    return [json.loads(report) for report in pattern.findall(app_output)]


@pytest.mark.requires_gpu
def test_the_encoded_channel_casts_and_carries_the_ordering_contract(
    start_app_under_test,
):
    """The encoded-domain link, read from Python: every bag libopus produced
    casts to an `EncodedAudioPacket`, and what the cast then reports is the
    wire contract the plan fixed.

    Every Opus packet is a sync point, so unlike the video probe this one
    enters at the first bag it sees and the ordering assertion is the whole
    of the doctrine: a `sequence_index` step other than exactly one is loss,
    and each packet is its own group.
    """
    app = start_app_under_test(OPUS_BLOCKS_APP)
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    app.await_marker("ENCODED_PACKETS_COMPLETE")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    packets = _reported(ENCODED_PACKET, app.output)
    assert len(packets) == ENCODED_PACKETS_REPORTED, (
        f"the probe reported {len(packets)} packets, not "
        f"{ENCODED_PACKETS_REPORTED}; output:\n{app.output}"
    )

    for packet in packets:
        assert packet["codec"] == "opus", (
            "the bag names the elementary stream its bitstream actually is"
        )
        assert packet["is_sync_point"] is True, (
            "a decoder enters an Opus stream at any packet, so the flag is a "
            "constant of the convention"
        )
        assert packet["sample_rate"] == 48_000, (
            "Opus codes at its own clock whatever the source was resampled from"
        )
        assert packet["channels"] == SOURCE_CHANNELS, (
            "the encoder declares no channel count, so the packet carries the "
            "source's own"
        )
        assert packet["sample_count"] == SAMPLES_IN_ONE_OPUS_PACKET, (
            "the input port's window contract frames at 20 ms, so every packet "
            "spans 960 per-channel samples"
        )
        assert packet["pre_skip"] in PRE_SKIP_SAMPLES_A_CREDIBLE_ENCODER_REPORTS, (
            "`pre_skip` is the minted encoder's reported lookahead — a zero "
            f"would mean it was never asked, and {packet['pre_skip']} past a "
            "packet is not a lookahead"
        )
        assert packet["byte_count"] > 0, "a packet with no bytes decodes to nothing"

    for earlier, later in zip(packets, packets[1:]):
        assert later["sequence_index"] == earlier["sequence_index"] + 1, (
            "`sequence_index` is monotonic in publication order and never "
            f"resets, so the step {earlier['sequence_index']} → "
            f"{later['sequence_index']} is loss on the link"
        )
        assert later["group_index"] == earlier["group_index"] + 1, (
            "every packet is a sync point, so every packet is its own group"
        )


@pytest.mark.requires_gpu
def test_the_decoded_blocks_carry_the_sources_format_and_the_trimmed_priming(
    start_app_under_test,
):
    """The far side of the pair: what libopus reconstructed, read back as
    ordinary audio blocks.

    The first block is the assertion that matters. The decoder trims the
    encoder's lookahead at entry, so that block is short by exactly `pre_skip`
    and is stamped at the entry packet's own instant — a decoder that skipped
    the trim would emit a full 960 there, and one that trimmed but then copied
    each packet's stamp would put every block a lookahead later than the audio
    it holds. From the second block on the stream is uniform: 960 samples,
    20 ms apart.
    """
    app = start_app_under_test(OPUS_BLOCKS_APP)
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    app.await_every_marker("ENCODED_PACKETS_COMPLETE", "DECODED_BLOCKS_COMPLETE")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    blocks = _reported(DECODED_BLOCK, app.output)
    assert len(blocks) == DECODED_BLOCKS_REPORTED, (
        f"the probe reported {len(blocks)} blocks, not "
        f"{DECODED_BLOCKS_REPORTED}; output:\n{app.output}"
    )
    packets = _reported(ENCODED_PACKET, app.output)
    assert packets, f"the encoded link reported nothing to compare against:\n{app.output}"

    for block in blocks:
        assert block["sample_rate"] == SOURCE_SAMPLE_RATE, (
            "a decoder reconstructs at Opus's own 48 kHz clock"
        )
        assert block["channels"] == SOURCE_CHANNELS, (
            "the decoded block carries the packet's own channel count, which "
            "followed the source's"
        )
        assert block["dtype"] == "f32", "libopus reconstructs float samples"
        assert block["sample_count"] * block["channels"] == block["scalars_read"], (
            "the block's declared count and the samples it actually carries "
            "have to be the same fact"
        )

    # The probe attaches at its own pace, so the first block it saw is the
    # decoder's first only when the decoder's entry packet is one the encoded
    # probe also wrote down. Bounded that way rather than assumed.
    entry_packet = next(
        (packet for packet in packets if packet["timestamp_ns"] == blocks[0]["timestamp_ns"]),
        None,
    )
    assert entry_packet is not None, (
        f"the first decoded block is stamped {blocks[0]['timestamp_ns']}, which "
        "rode no encoded packet this run wrote down — either the decoder "
        "re-stamped it at publication, or the two probes' windows did not "
        f"overlap; output:\n{app.output}"
    )
    assert blocks[0]["sample_count"] == (
        SAMPLES_IN_ONE_OPUS_PACKET - entry_packet["pre_skip"]
    ), (
        "the first block after entry is short by exactly the encoder's "
        "lookahead — the decoder trims the priming so its first emitted sample "
        "is the stamped instant"
    )

    for block in blocks[1:]:
        assert block["sample_count"] == SAMPLES_IN_ONE_OPUS_PACKET, (
            "only the entry block is trimmed; the decoder holds nothing back "
            "and re-frames nothing, so every later block is one whole packet"
        )
    for earlier, later in zip(blocks[1:], blocks[2:]):
        assert (
            later["timestamp_ns"] - earlier["timestamp_ns"]
        ) == NANOSECONDS_PER_OPUS_PACKET, (
            "past the trimmed entry block the stamps are exactly 20 ms apart, "
            "derived from the anchor rather than read off a clock: "
            f"{earlier['timestamp_ns']} → {later['timestamp_ns']}"
        )

    assert blocks[1]["timestamp_ns"] - blocks[0]["timestamp_ns"] == (
        blocks[0]["sample_count"] * 1_000_000_000 // SOURCE_SAMPLE_RATE
    ), (
        "the second block starts exactly where the short first one ended, "
        "which is what makes the trim a discarded lookahead rather than a hole "
        "in the audio"
    )
