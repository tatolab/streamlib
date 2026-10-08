# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The streams `test_capability_contexts.py` starts on `tatolabd`.

A context probe reports from its own hooks, so its stream is the probe alone;
a timestamp or worker-thread scenario is a source into the sink that reports
what it read.
"""

from tatolab.stream import StreamBuilder, stream

import capability_context_probes
from zero_argument_process_processor import ZeroArgumentProcess


def _add_a_source_into_a_reporting_sink(
    stream_builder: StreamBuilder, source_class: type, reporting_sink_class: type
) -> None:
    source = stream_builder.add(source_class)
    sink = stream_builder.add(reporting_sink_class)
    stream_builder.connect(source.output("bags_to_downstream"), sink.input("bags_from_upstream"))


@stream
def setup_context_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.SetupContextProbe)


@stream
def process_context_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.ProcessContextProbe)


@stream
def config_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.ConfigProbe)


@stream
def time_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.TimeProbe)


@stream
def context_stasher_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.ContextStasher)


@stream
def pixel_buffer_acquirer_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.PixelBufferAcquirer)


@stream
def repeated_pixel_buffer_acquirer_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.RepeatedPixelBufferAcquirer)


@stream
def worker_thread_privileged_constructor_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(capability_context_probes.WorkerThreadPrivilegedConstructor)


@stream
def one_config_probe_with_gain_and_label(stream_builder: StreamBuilder) -> None:
    """A `ConfigProbe` configured with a gain and a label."""
    stream_builder.add(capability_context_probes.ConfigProbe, config={"gain": 2.5, "label": "left"})


@stream
def an_explicitly_stamped_source_into_a_timestamp_collecting_sink(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_source_into_a_reporting_sink(
        stream_builder,
        capability_context_probes.ExplicitlyStampedSource,
        capability_context_probes.TimestampCollectingSink,
    )


@stream
def a_default_stamped_source_into_a_timestamp_collecting_sink(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_source_into_a_reporting_sink(
        stream_builder,
        capability_context_probes.DefaultStampedSource,
        capability_context_probes.TimestampCollectingSink,
    )


@stream
def a_worker_thread_source_into_a_worker_thread_bag_sink(stream_builder: StreamBuilder) -> None:
    _add_a_source_into_a_reporting_sink(
        stream_builder,
        capability_context_probes.WorkerThreadSource,
        capability_context_probes.WorkerThreadBagSink,
    )


@stream
def zero_argument_process(stream_builder: StreamBuilder) -> None:
    """The one processor whose `process` takes no ctx."""
    stream_builder.add(ZeroArgumentProcess)
