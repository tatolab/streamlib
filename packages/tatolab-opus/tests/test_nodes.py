# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two nodes: their declarations, and a tone through both over real links.

The links are helper-process link endpoints wired directly, the seam `setup()`
and `process()` see in a real child, so nothing here needs a runtime or a
device. `tatolab.runtime` is borrowed from the runtime unit's lend.
"""

import math
import os
import struct
from typing import Any

import pytest
from helper_process_link_wiring import wire_one_link
from parent_catalog_entry_stand_in import ParentHoldingCatalogEntries
from tatolab.opus import OpusDecoder, OpusEncoder
from tatolab.runtime import _engine
from tatolab.runtime._processor_hosting import construct_processor_instance
from tatolab.runtime._processor_interpreter_bootstrap import describe_one_processor_class
from tatolab.stream import AudioBlock, StreamBuilder, compile_stream_to_graph, log, stream

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

OPUS_SAMPLE_RATE_HZ = 48_000
SAMPLES_IN_ONE_WINDOW = 960
NANOSECONDS_PER_WINDOW = SAMPLES_IN_ONE_WINDOW * 1_000_000_000 // OPUS_SAMPLE_RATE_HZ
# libopus's lookahead at 48 kHz, `Fs/400 + Fs/250`.
LIBOPUS_PRE_SKIP_SAMPLES = 312


@stream
def an_opus_round_trip(stream_builder: StreamBuilder) -> None:
    encoder = stream_builder.add(OpusEncoder, config={"bitrate_bps": 64_000})
    decoder = stream_builder.add(OpusDecoder)
    stream_builder.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))


def nodes_by_type(graph: "dict[str, Any]") -> "dict[str, dict[str, Any]]":
    return {node["type"]: node for node in graph["nodes"]}


def test_each_node_is_named_by_its_own_import_path_in_the_module():
    graph = compile_stream_to_graph(an_opus_round_trip)

    assert set(nodes_by_type(graph)) == {"tatolab.opus:OpusEncoder", "tatolab.opus:OpusDecoder"}
    assert nodes_by_type(graph)["tatolab.opus:OpusEncoder"]["config"] == {"bitrate_bps": 64_000}


def test_the_encoders_input_declares_a_window_contract_that_follows_its_sources_channel_count():
    [audio] = describe_one_processor_class("tatolab.opus:OpusEncoder")["input_ports"]

    assert audio["name"] == "audio"
    assert audio["delivery_profile"] == "ordered"
    assert audio["audio_window"] == {
        "resolved_from": "declaration",
        "sample_rate": OPUS_SAMPLE_RATE_HZ,
        "channels": "source",
        "dtype": "f32",
        "window_size": SAMPLES_IN_ONE_WINDOW,
        "hop": SAMPLES_IN_ONE_WINDOW,
    }


def test_the_decoders_output_states_a_msgpack_snapshot_entry():
    [audio] = describe_one_processor_class("tatolab.opus:OpusDecoder")["output_ports"]

    assert audio["name"] == "audio"
    assert audio["catalog_entry"] == {
        "section": "binary",
        "entry": {"mode": "snapshot", "mime": "application/msgpack"},
    }


@pytest.mark.parametrize(
    "config, refusal",
    [
        ({"application": "surround"}, "surround"),
        ({"bitrate_bps": 0}, "bitrate_bps"),
        ({"bitrate_bps": True}, "bitrate_bps"),
        ({"bitrate_bps": 2**32}, "bitrate_bps"),
    ],
)
def test_an_encoder_config_libopus_cannot_take_is_refused_by_name(config, refusal):
    with pytest.raises(ValueError, match=refusal):
        construct_processor_instance(OpusEncoder, config, None)


def test_an_empty_encoder_config_is_legal():
    assert isinstance(construct_processor_instance(OpusEncoder, {}, None), OpusEncoder)


def test_an_encoder_setting_the_class_does_not_take_is_refused():
    with pytest.raises(TypeError, match="channels"):
        construct_processor_instance(OpusEncoder, {"channels": 2}, None)


def a_stereo_tone_window(window_index: int) -> AudioBlock:
    """20 ms of a tone per channel at Opus's rate, stamped on the window grid."""
    scalars = []
    for sample in range(SAMPLES_IN_ONE_WINDOW):
        instant = (window_index * SAMPLES_IN_ONE_WINDOW + sample) / OPUS_SAMPLE_RATE_HZ
        scalars.append(0.4 * math.sin(2 * math.pi * 220 * instant))
        scalars.append(0.4 * math.sin(2 * math.pi * 440 * instant))
    return AudioBlock(
        samples=struct.pack(f"<{len(scalars)}f", *scalars),
        sample_rate=OPUS_SAMPLE_RATE_HZ,
        channels=2,
        sample_count=SAMPLES_IN_ONE_WINDOW,
        first_sample_timestamp_ns=window_index * NANOSECONDS_PER_WINDOW,
        dtype="f32",
    )


class OpusRoundTripOnWiredLinks:
    """A source endpoint into `OpusEncoder`, its `encoded_audio` into
    `OpusDecoder`, and the decoder's `audio` into a sink endpoint read here."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        unique = f"opus{os.getpid()}_{request.node.name}"
        self.parent = ParentHoldingCatalogEntries()
        self.log_records: "list[tuple[str, str]]" = []
        log.install_helper_process_sink(
            lambda level, message, attrs: self.log_records.append((level, message))
        )

        self.source = _engine.open_node_link_data_access_for_helper_process()
        encoder_links = _engine.open_node_link_data_access_for_helper_process()
        decoder_links = _engine.open_node_link_data_access_for_helper_process()
        sink_links = _engine.open_node_link_data_access_for_helper_process()
        wire_one_link(
            self.source, "audio", encoder_links, "audio", f"{unique}/source", f"{unique}_e/notify"
        )
        wire_one_link(
            encoder_links,
            "encoded_audio",
            decoder_links,
            "encoded_audio",
            f"{unique}/encoder",
            f"{unique}_d/notify",
        )
        self.parent.downstream_input_port_by_output_port["encoded_audio"] = "encoded_audio"
        wire_one_link(
            decoder_links, "audio", sink_links, "audio", f"{unique}/decoder", f"{unique}_s/notify"
        )

        def context(links: Any, processor_id: str) -> Any:
            return _engine.open_runtime_context_full_access_for_helper_process(
                {},
                links,
                "runtime-under-test",
                processor_id,
                self.parent.escalate_request_to_parent,
            )

        self.encoder_context = context(encoder_links, "encoder-under-test")
        self.decoder_context = context(decoder_links, "decoder-under-test")
        self.sink_context = context(sink_links, "sink-under-test")
        self.encoder = construct_processor_instance(
            OpusEncoder, {"bitrate_bps": 128_000}, encoder_links
        )
        self.decoder = construct_processor_instance(OpusDecoder, {}, decoder_links)

    def send(self, window: AudioBlock) -> None:
        self.source.write_to_output_port(
            "audio", window.to_bytes(), window.first_sample_timestamp_ns, True
        )
        self.encoder.process(
            _engine.limited_access_view_of_runtime_context_for_helper_process(self.encoder_context)
        )
        self.decoder.process(
            _engine.limited_access_view_of_runtime_context_for_helper_process(self.decoder_context)
        )

    def decoded_blocks(self) -> "list[AudioBlock]":
        blocks = []
        while (block := self.sink_context.inputs.read("audio", into=AudioBlock)) is not None:
            blocks.append(block)
        return blocks


@pytest.fixture
def opus_round_trip(request) -> OpusRoundTripOnWiredLinks:
    return OpusRoundTripOnWiredLinks(request)


def test_the_encoder_states_its_ports_entry_with_the_opus_head_before_the_first_packet(
    opus_round_trip: OpusRoundTripOnWiredLinks,
):
    opus_round_trip.send(a_stereo_tone_window(0))

    [(port, section, entry)] = opus_round_trip.parent.catalog_entries_stated
    assert (port, section) == ("encoded_audio", "audio")
    assert entry["codec"] == "opus"
    assert entry["sampleRate"] == OPUS_SAMPLE_RATE_HZ
    assert entry["numberOfChannels"] == 2
    opus_head = bytes.fromhex(entry["description"])
    assert opus_head[:8] == b"OpusHead"
    assert opus_head[9] == 2
    assert int.from_bytes(opus_head[10:12], "little") == LIBOPUS_PRE_SKIP_SAMPLES
    assert any("minted the Opus encoder" in message for _, message in opus_round_trip.log_records)


def test_a_tone_comes_back_as_audio_blocks_trimmed_and_stamped_from_the_entry_packet(
    opus_round_trip: OpusRoundTripOnWiredLinks,
):
    windows = 6
    for window_index in range(windows):
        opus_round_trip.send(a_stereo_tone_window(window_index))

    blocks = opus_round_trip.decoded_blocks()

    assert len(blocks) == windows
    assert [block.channels for block in blocks] == [2] * windows
    assert {block.sample_rate for block in blocks} == {OPUS_SAMPLE_RATE_HZ}
    assert blocks[0].sample_count == SAMPLES_IN_ONE_WINDOW - LIBOPUS_PRE_SKIP_SAMPLES
    assert blocks[0].first_sample_timestamp_ns == 0
    emitted = 0
    for block in blocks:
        assert block.first_sample_timestamp_ns == emitted * 1_000_000_000 // OPUS_SAMPLE_RATE_HZ
        emitted += block.sample_count
    # After the priming, the reconstruction carries the tone rather than silence.
    later = struct.unpack(f"<{blocks[-1].sample_count * 2}f", blocks[-1].interleaved_sample_bytes)
    assert max(abs(scalar) for scalar in later) > 0.2


def test_a_window_whose_channel_count_opus_cannot_place_is_refused_by_name_and_then_discarded(
    opus_round_trip: OpusRoundTripOnWiredLinks,
):
    nine_channels = AudioBlock(
        samples=bytes(SAMPLES_IN_ONE_WINDOW * 9 * 4),
        sample_rate=OPUS_SAMPLE_RATE_HZ,
        channels=9,
        sample_count=SAMPLES_IN_ONE_WINDOW,
        first_sample_timestamp_ns=0,
    )

    with pytest.raises(ValueError, match="9 channels resolve to no Opus stream layout"):
        opus_round_trip.send(nine_channels)
    opus_round_trip.send(nine_channels)

    assert opus_round_trip.parent.catalog_entries_stated == []
    assert opus_round_trip.decoded_blocks() == []


def test_a_message_on_the_decoders_link_that_is_no_media_frame_is_refused_by_name(
    opus_round_trip: OpusRoundTripOnWiredLinks,
):
    decoder = construct_processor_instance(OpusDecoder, {}, None)
    with pytest.raises(ValueError, match="not a media frame"):
        decoder._decoder.decode_one_arriving_message(b"", 0, True, 0, None)
    with pytest.raises(ValueError, match="names codec `mp4a.40.2`"):
        decoder._decoder.decode_one_arriving_message(
            b"\x00\xfc\xff\xfe",
            0,
            True,
            0,
            {"section": "audio", "entry": {"codec": "mp4a.40.2", "numberOfChannels": 2}},
        )
    with pytest.raises(ValueError, match="`video` entry"):
        decoder._decoder.decode_one_arriving_message(
            b"\x00\xfc\xff\xfe", 0, True, 0, {"section": "video", "entry": {"codec": "avc1"}}
        )
