# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that report which process they are running in.

The test process imports this module to compile a stream; every other import
of it is a processor interpreter's, describing a class or running one. With
`STREAMLIB_TEST_MODULE_IMPORT_RECORD_DIRECTORY` set, each import records its
own pid and its parent's there — the witness that `tatolabd` never imported it.
"""

import dataclasses
import json
import os
import time

from tatolab.stream import log, node
from tatolab.stream.msgpack import pack, unpack

#: A payload no msgpack decoder reads: `0xc1` is the one type byte msgpack never
#: uses, then every byte value once, so an encode or decode anywhere between
#: the writer and the reader changes or refuses it.
RAW_MESSAGE_PAYLOAD_BYTES = bytes([0xC1]) + bytes(range(256))

#: Names the directory each import of this module records `<pid>` in, holding
#: the importing process's parent pid.
MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_TEST_MODULE_IMPORT_RECORD_DIRECTORY"

if MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE in os.environ:
    with open(
        os.path.join(os.environ[MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE], str(os.getpid())),
        "w",
    ) as module_import_record:
        module_import_record.write(str(os.getppid()))


def report(marker_name: str, payload: "dict[str, object] | None" = None) -> None:
    log.info(f"MARKER:{marker_name}" + ("" if payload is None else " " + json.dumps(payload)))


@dataclasses.dataclass
class ReportsItsOwnProcessSourceConfig:
    label: str = "unlabelled"


@node(execution="continuous", interval_ms=10)
class ReportsItsOwnProcessSource:
    """Stamps every message with the pid it was produced in."""

    def __init__(self, config: ReportsItsOwnProcessSourceConfig) -> None:
        self.label = config.label
        self.announced = False

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if not self.announced:
            report("SOURCE_PID", {"label": self.label, "pid": os.getpid()})
            self.announced = True
        ctx.outputs.write("frames_to_downstream", pack({"produced_in_pid": os.getpid()}))


@node
class ReportsUpstreamProcessSink:
    """Announces the pid a message was produced in, alongside its own."""

    def __init__(self) -> None:
        self.messages_seen = 0

    @node.input(delivery_profile="newest")
    def frames_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        payload = ctx.inputs.read("frames_from_upstream")
        if payload is None:
            return
        # Announced on every tenth message rather than once: a test that waits
        # for this *after* another processor crashed needs evidence of live
        # traffic, not a marker emitted before the crash.
        self.messages_seen += 1
        if self.messages_seen % 10 == 1:
            upstream_pid = unpack(payload)["produced_in_pid"]
            report("SINK_PID", {"sink_pid": os.getpid(), "upstream_pid": upstream_pid})


@node
class ReportsItsOwnProcessVideoSink:
    """Announces its own process, reading frames a native built-in produced.

    The frames come from `TestPatternSource`, which is native and therefore
    has no process of its own — so this sink's pid, set against `tatolabd`'s,
    is what discriminates the two sides of the boundary.
    """

    def __init__(self) -> None:
        self.announced = False

    @node.input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if ctx.inputs.read("video_from_upstream") is None or self.announced:
            return
        self.announced = True
        report("VIDEO_SINK_PID", {"pid": os.getpid()})


@node(execution="continuous", interval_ms=10)
class WritesRawMessagePayloadSource:
    """Writes `RAW_MESSAGE_PAYLOAD_BYTES` every tick, after reporting once what
    handing `write` a dict raised."""

    def __init__(self) -> None:
        self.reported_the_dict_write = False

    @node.output()
    def raw_messages_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if not self.reported_the_dict_write:
            self.reported_the_dict_write = True
            try:
                ctx.outputs.write("raw_messages_to_downstream", {"not": "bytes"})
            except Exception as refusal:  # noqa: BLE001 — the refusal is the observation
                refusal_type, refusal_text = type(refusal).__name__, str(refusal)
            else:
                refusal_type, refusal_text = None, None
            report(
                "DICT_WRITE_OUTCOME",
                {
                    "source_pid": os.getpid(),
                    "refusal_type": refusal_type,
                    "refusal_text": refusal_text,
                },
            )
        ctx.outputs.write("raw_messages_to_downstream", RAW_MESSAGE_PAYLOAD_BYTES)


@node
class ReportsTheRawMessagePayloadItReadSink:
    """Reports the first payload it reads, as hex, with its type and its own pid."""

    def __init__(self) -> None:
        self.reported = False

    @node.input(delivery_profile="ordered")
    def raw_messages_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        payload = ctx.inputs.read("raw_messages_from_upstream")
        if payload is None or self.reported:
            return
        self.reported = True
        report(
            "RAW_MESSAGE_PAYLOAD_READ",
            {
                "sink_pid": os.getpid(),
                "payload_type": type(payload).__name__,
                "payload_hex": bytes(payload).hex(),
            },
        )


@node(execution="continuous", interval_ms=10)
class DiesAbruptlyProbe:
    """Takes its own process down mid-run, the way a segfaulting native call
    inside a user callback would."""

    def __init__(self) -> None:
        self.frames_before_dying = 3

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        self.frames_before_dying -= 1
        if self.frames_before_dying <= 0:
            report("ABOUT_TO_DIE", {"pid": os.getpid()})
            # Not an exception: the point is a process that stops existing
            # without unwinding, which is what a segfaulting native call does.
            os._exit(1)


@node(execution="continuous", interval_ms=10)
class SleepsThroughItsOwnShutdownProbe:
    """Parks in `process()` far past the ladder's one-second budget.

    The bag it had in flight is lost to the interrupt, and `stop()` and
    `teardown()` still run — which is what the markers below are for.
    """

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        report("ASLEEP_IN_PROCESS", {"pid": os.getpid()})
        time.sleep(30)
        report("SLEPT_THE_WHOLE_WAY")

    def stop(self, ctx) -> None:
        report("SLEEPER_STOPPED")

    def teardown(self, ctx) -> None:
        report("SLEEPER_TORE_DOWN")


@node(execution="continuous", interval_ms=50)
class ForksAWorkerThatOutlivesItProbe:
    """Starts a worker of its own the way `os.system("sleep 60 &")` does.

    The worker is the descendant the process-group rung exists to reach. Its
    pid is announced so a test can go looking for it after `tatolabd` is gone.
    """

    def __init__(self) -> None:
        self.worker_pid = None

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.worker_pid is None:
            self.worker_pid = os.fork()
            if self.worker_pid == 0:
                time.sleep(120)
                os._exit(0)
            report("WORKER_PID", {"worker_pid": self.worker_pid, "helper_pid": os.getpid()})


@node(execution="manual")
class SleepsThroughItsOwnSetupProbe:
    """Parks in `setup()`, so shutdown finds it still registering.

    That is the one route onto the ladder the engine's `stop()` hook never
    reaches, and the plan still owes this processor its `teardown()`.
    """

    @node.output()
    def frames_to_downstream(self) -> None: ...

    def setup(self, ctx) -> None:
        report("ASLEEP_IN_SETUP", {"pid": os.getpid()})
        time.sleep(120)
        report("SLEPT_THE_WHOLE_SETUP")

    def teardown(self, ctx) -> None:
        report("INTERRUPTED_SETUP_TORE_DOWN")

