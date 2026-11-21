# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The ports a helper process reads and writes: the links the engine wires,
the islands and pin values that feed them, and links wired and unwired mid-run.
"""

import json
import time

import pytest

from helper_process_test_support import (
    PROBE_MODULE,
    drive_lifecycle_on_a_thread,
    engine_shaped_link_wiring,
    frame_index_payload,
    the_pin_upstream_of_a_pass_through_probe,
)
from tatolab.runtime import _engine
from tatolab.runtime import _node_interpreter_bootstrap as node_interpreter_bootstrap
from tatolab.runtime._node_interpreter_bootstrap import (
    ParentProcessBridge,
    load_node_class,
)
from tatolab.stream import NodeLinkDataAccess

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def payload_bytes_read_from(destination: NodeLinkDataAccess, input_port: str) -> bytes:
    """The next message's payload on `input_port` as a copy, raising when none waits."""
    read = destination.read_from_input_port(input_port)
    assert read is not None, f"no message waited on {input_port!r}"
    return bytes(read)


def test_a_declared_port_with_no_link_reads_empty_and_drops_writes():
    """A node added to a running graph is set up before any link reaches
    it, and a link may be taken away again later. Its declared ports stay
    usable throughout: a read finds nothing, a write goes nowhere, and neither
    raises. Only a port the class never declared is refused, by name.
    """
    link_data_access = _engine.open_node_link_data_access_for_helper_process()
    link_data_access.declare_ports(["frames_from_upstream"], ["frames_to_downstream"])

    assert link_data_access.read_from_input_port("frames_from_upstream") is None
    assert link_data_access.input_port_has_data("frames_from_upstream") is False
    link_data_access.write_to_output_port("frames_to_downstream", frame_index_payload(1))

    with pytest.raises(RuntimeError, match="never_declared"):
        link_data_access.read_from_input_port("never_declared")
    with pytest.raises(RuntimeError, match="never_declared"):
        link_data_access.write_to_output_port("never_declared", frame_index_payload(1))


def test_a_port_lookup_casts_its_argument_to_the_declared_name():
    """`@node` declares each port under its cast name, so every lookup casts
    too: `Video` and `VIDEO` reach the port declared as `video`.
    """
    link_data_access = _engine.open_node_link_data_access_for_helper_process()
    link_data_access.declare_ports(["video"], ["frames-out"])

    assert link_data_access.read_from_input_port("Video") is None
    assert link_data_access.read_from_input_port_with_timestamp("VIDEO") == (None, None)
    assert link_data_access.input_port_has_data("Vidéo") is False
    link_data_access.write_to_output_port("Frames Out", frame_index_payload(1))

    with pytest.raises(ValueError, match="cannot name anything"):
        link_data_access.read_from_input_port("..")


def test_a_helper_opens_its_own_ports_from_the_envelope_the_engine_sends():
    """The whole point of the wiring envelope: two helpers open their own ends
    of one channel from what the parent sent, and a message crosses between
    them as the same bytes.

    Both planes live on this thread because iceoryx2's ports are `!Send`, and
    the destination is wired first because a send with no subscriber attached
    is dropped.
    """
    link_id = "L-envelope-test"
    destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        source, {"outputs": [engine_shaped_link_wiring("output", link_id)]}
    )

    source.write_to_output_port("frames_to_downstream", frame_index_payload(11))

    assert destination.any_input_port_has_data()
    assert payload_bytes_read_from(destination, "frames_from_upstream") == frame_index_payload(11)


def test_a_message_read_unread_is_handed_on_exactly_as_it_arrived():
    """A function node's helper hands a message on without reading it: the
    same payload, stamp and group flag reach the next reader."""
    from tatolab.runtime._engine import (
        hand_an_unread_message_on,
        read_the_message_at_an_input_port_unread,
    )

    into_the_relay = "L-unread-into-the-relay"
    out_of_the_relay = "L-unread-out-of-the-relay"
    destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [engine_shaped_link_wiring("input", out_of_the_relay)]}
    )
    relay = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        relay,
        {
            "inputs": [engine_shaped_link_wiring("input", into_the_relay)],
            "outputs": [engine_shaped_link_wiring("output", out_of_the_relay)],
        },
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        source, {"outputs": [engine_shaped_link_wiring("output", into_the_relay)]}
    )
    assert (
        read_the_message_at_an_input_port_unread(
            link_data_access=relay, input_port="frames_from_upstream"
        )
        is None
    )

    source.write_to_output_port(
        "frames_to_downstream", frame_index_payload(5), timestamp_ns=123_456, starts_group=False
    )
    message = read_the_message_at_an_input_port_unread(
        link_data_access=relay, input_port="frames_from_upstream"
    )
    assert message is not None
    hand_an_unread_message_on(
        link_data_access=relay, output_port="frames_to_downstream", message=message
    )

    payload, timestamp_ns = destination.read_from_input_port_with_timestamp("frames_from_upstream")
    assert payload is not None
    assert bytes(payload) == frame_index_payload(5)
    assert timestamp_ns == 123_456


def test_a_helper_opens_the_channel_at_its_creation_depth_and_reads_at_its_own_ports_depth():
    """A helper opens a channel at the creation depth the envelope names and
    reads through a ring its own port's depth, so a shallow port that opens a
    channel first never sizes it for the deeper one that joins.

    Ten messages published while neither reads: the four-deep port reads four,
    the sixteen-deep port reads all ten. A service opened at the port's ring
    depth would refuse the second destination's open
    (`DoesNotSupportRequestedMinBufferSize`).
    """

    messages_published_while_neither_reads = 10
    shallow_wiring = engine_shaped_link_wiring("input", "L-shallow-port")
    shallow_wiring["input_port_ring_depth"] = 4
    deep_wiring = engine_shaped_link_wiring("input", "L-deep-port")
    deep_wiring["channel_service_name"] = shallow_wiring["channel_service_name"]
    # Both names move together: a link from this runtime is known by the
    # channel it subscribed to, so leaving the name behind would send an
    # envelope the engine never emits.
    deep_wiring["inbound_link_name"] = shallow_wiring["channel_service_name"]
    deep_wiring["notify_service_name"] = f"{shallow_wiring['notify_service_name']}_deep"
    source_wiring = engine_shaped_link_wiring("output", "L-shallow-port")
    source_wiring["channel_service_name"] = shallow_wiring["channel_service_name"]
    for wiring in (shallow_wiring, deep_wiring, source_wiring):
        wiring["max_subscribers"] = 3

    shallow_destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        shallow_destination, {"inputs": [shallow_wiring]}
    )
    deep_destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        deep_destination, {"inputs": [deep_wiring]}
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(source, {"outputs": [source_wiring]})

    for frame_index in range(messages_published_while_neither_reads):
        source.write_to_output_port("frames_to_downstream", frame_index_payload(frame_index))

    def every_frame_index_read(destination: NodeLinkDataAccess) -> list:
        read = []
        while (payload := destination.read_from_input_port("frames_from_upstream")) is not None:
            read.append(json.loads(bytes(payload))["frame_index"])
        return read

    assert every_frame_index_read(shallow_destination) == [6, 7, 8, 9]
    assert every_frame_index_read(deep_destination) == list(
        range(messages_published_while_neither_reads)
    )


def test_a_disconnected_links_ports_are_free_for_its_reconnect():
    """A disconnect must release the ports this process opened, or the same
    link cannot be wired a second time.

    This envelope caps the notify service at one notifier (the engine sends its
    fixed inbound-link cap; the fixture shrinks it), so a notifier still held
    from the first connect makes the reconnect's `create_notifier` the second
    on a max-1 service — `ExceedsMaxSupportedNotifiers`. The engine cannot reclaim these
    from the parent: the publisher, notifier and subscriber all belong here.

    The Python-surface mirror of the engine's
    `disconnect_reconnect_cycle_reclaims_notifier_and_data_service`.
    """
    link_id = "L-reconnect-cycle"
    destination = _engine.open_node_link_data_access_for_helper_process()
    source = _engine.open_node_link_data_access_for_helper_process()

    # The destination is wired first both times: a send with no subscriber
    # attached is dropped.
    def connect() -> None:
        node_interpreter_bootstrap.wire_link_data_access(
            destination, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
        )
        node_interpreter_bootstrap.wire_link_data_access(
            source, {"outputs": [engine_shaped_link_wiring("output", link_id)]}
        )

    def disconnect() -> None:
        node_interpreter_bootstrap.unwire_link_data_access(
            source,
            {
                "direction": "output",
                "port": "frames_to_downstream",
                "link_id": link_id,
            },
        )
        node_interpreter_bootstrap.unwire_link_data_access(
            destination, {"direction": "input", "link_id": link_id}
        )

    connect()
    disconnect()
    connect()

    source.write_to_output_port("frames_to_downstream", frame_index_payload(13))
    assert payload_bytes_read_from(destination, "frames_from_upstream") == frame_index_payload(13), (
        "the reconnected link must carry data through freshly opened ports"
    )


def test_unwiring_a_link_in_an_unknown_direction_touches_neither_plane():
    """A direction this side does not know is a parent that has drifted from
    this protocol. It must take nothing down and release nothing — guessing a
    direction would drop a port that is still carrying frames.
    """
    link_id = "L-unknown-direction"
    destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        source, {"outputs": [engine_shaped_link_wiring("output", link_id)]}
    )

    node_interpreter_bootstrap.unwire_link_data_access(
        source,
        {
            "direction": "sideways",
            "port": "frames_to_downstream",
            "link_id": link_id,
        },
    )
    node_interpreter_bootstrap.unwire_link_data_access(
        destination, {"direction": "sideways", "link_id": link_id}
    )

    source.write_to_output_port("frames_to_downstream", frame_index_payload(14))
    assert payload_bytes_read_from(destination, "frames_from_upstream") == frame_index_payload(14), (
        "an unrecognised direction must leave both ends of the link wired"
    )


def test_unwire_link_is_dispatched_mid_run_and_answers_nothing(stand_in_parent):
    """The parent sends this from its compiler while it holds the graph write
    lock, so it cannot wait for a reply — and a reply nobody reads would be
    taken as the answer to the next command, exactly like one to `run`.

    It must also reach a child that is already in its execution loop, which is
    the only state a disconnect can arrive in.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    stand_in_parent.send(
        {
            "cmd": "unwire_link",
            "direction": "output",
            "port": "frames_to_downstream",
            "link_id": "L-mid-run",
        }
    )
    assert stand_in_parent.receive(timeout_seconds=0.5) is None

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "an unanswered unwire must leave the next command's reply the next thing "
        "the parent reads"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_link_wired_after_setup_opens_its_port_mid_run(stand_in_parent):
    """A node added to a running graph is set up with no links and gets
    its first ones from later connects. The engine hands each one over as a
    `wire_link` the child acts on from inside its execution loop — parked on
    the parent, because with no input it has nothing else to wait on — and
    bags then cross both new links.

    Fail-without-fix: drop the `wire_link` arm from `_dispatch` and the child
    logs an unknown command, its input never opens, and the bag published
    below never comes back.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    inbound_link_id = "L-wired-late-in"
    outbound_link_id = "L-wired-late-out"
    # The far end of the child's output is opened first, so nothing the child
    # publishes is dropped for want of a subscriber.
    downstream = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        downstream,
        {"inputs": [engine_shaped_link_wiring("input", outbound_link_id)]},
    )
    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "output",
            "link": engine_shaped_link_wiring("output", outbound_link_id),
        }
    )
    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "input",
            "link": engine_shaped_link_wiring("input", inbound_link_id),
        }
    )
    upstream = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        upstream,
        {"outputs": [engine_shaped_link_wiring("output", inbound_link_id)]},
    )

    wire_answers = {
        answer["link_id"]: answer
        for answer in (stand_in_parent.receive(), stand_in_parent.receive())
    }

    # A message published before the child's subscriber is up is dropped by
    # the channel, so publish until one comes back.
    upstream_pin = the_pin_upstream_of_a_pass_through_probe(upstream)
    deadline = time.monotonic() + 10.0
    forwarded = None
    while forwarded is None and time.monotonic() < deadline:
        upstream_pin.write(frame_index_payload(7))
        time.sleep(0.05)
        if downstream.any_input_port_has_data():
            forwarded = downstream.read_from_input_port("frames_from_upstream")
    assert forwarded is not None
    assert json.loads(bytes(forwarded)) == {"frame_index": 7, "tag": "untagged"}, (
        "a message must cross the input wired after setup and come back over the "
        "output wired after setup"
    )

    assert wire_answers == {
        outbound_link_id: {"rpc": "link_wired", "link_id": outbound_link_id},
        inbound_link_id: {"rpc": "link_wired", "link_id": inbound_link_id},
    }, "each wire is answered for the link it names, and both ports did open"

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "a wire's answer rides its own link-scoped tag, so the next lifecycle "
        "command's reply is still the next lifecycle frame the parent reads"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_helper_output_keeping_its_last_message_reaches_a_reader_linked_later(
    stand_in_parent,
):
    """A helper node that wrote once before any link reaches its output has
    that message read by the destination of the output's first link, wired
    after setup, with nothing written after it: the setup command names the
    port as one keeping its last message.

    Fail-without-fix: drop the setup command's list from the helper's writer,
    or drop the write to an unlinked port before the writer, and the
    destination never reads.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:WritesOnceInSetupProbe")
    )

    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {},
            "output_ports_keeping_their_last_message": ["frames_to_downstream"],
        }
    )
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 60_000})

    link_id = "L-kept-last-message"
    downstream = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        downstream, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
    )
    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "output",
            "link": engine_shaped_link_wiring("output", link_id),
        }
    )
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": link_id}

    deadline = time.monotonic() + 10.0
    while not downstream.any_input_port_has_data() and time.monotonic() < deadline:
        time.sleep(0.02)
    assert json.loads(payload_bytes_read_from(downstream, "frames_from_upstream")) == {
        "written": "once"
    }

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def an_island_feeding_the_probes_level(*, then_double_it: bool) -> dict:
    held = {
        "node": "held-1",
        "breadcrumb_number": 0,
        "import_path": f"{PROBE_MODULE}:hold_a_level",
        "config": None,
        "links_into": [],
    }
    nodes = [held]
    output_node = "held-1"
    if then_double_it:
        nodes.append(
            {
                "node": "doubled-1",
                "breadcrumb_number": 1,
                "import_path": f"{PROBE_MODULE}:double_a_level",
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "held-1", "port": "result"}}],
            }
        )
        output_node = "doubled-1"
    return {
        "input_port": "level",
        "link_id": "L-island-into-level",
        "output": {"node": output_node, "port": "result"},
        "nodes": nodes,
    }


def wait_until_the_island_reading_probe_reads(level, timeout_s: float = 10.0) -> None:
    import helper_process_probes

    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if helper_process_probes.LEVELS_THE_ISLAND_READING_PROBE_READ[-3:] == [level] * 3:
            return
        time.sleep(0.01)
    raise AssertionError(
        f"the probe never read {level!r} three times running; its last reads: "
        f"{helper_process_probes.LEVELS_THE_ISLAND_READING_PROBE_READ[-5:]}"
    )


def test_an_island_fed_after_setup_is_evaluated_swapped_and_stopped_mid_run(stand_in_parent):
    """A function node linked into a running reader reaches it as a
    `feed_island`, answered on the island's link like a late `wire_link`; an
    edit inside the island is the same frame again, and unlinking it is an
    unanswered `unfeed_island`.

    Fail-without-fix: drop the `feed_island` arm from `_dispatch` and the
    parent hears no answer, the link stays pending, and the probe never reads
    the island's value.
    """
    from helper_process_probes import LEVELS_THE_ISLAND_READING_PROBE_READ

    LEVELS_THE_ISLAND_READING_PROBE_READ.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:IslandReadingProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})

    stand_in_parent.send(
        {"cmd": "feed_island", "island": an_island_feeding_the_probes_level(then_double_it=False)}
    )
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": "L-island-into-level"}
    wait_until_the_island_reading_probe_reads(0.75)

    stand_in_parent.send(
        {"cmd": "feed_island", "island": an_island_feeding_the_probes_level(then_double_it=True)}
    )
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": "L-island-into-level"}
    wait_until_the_island_reading_probe_reads(1.5)

    unloadable = an_island_feeding_the_probes_level(then_double_it=False)
    unloadable["nodes"][0]["import_path"] = f"{PROBE_MODULE}:no_such_function_node"
    stand_in_parent.send({"cmd": "feed_island", "island": unloadable})
    refusal = stand_in_parent.receive()
    assert refusal["rpc"] == "link_wire_failed"
    assert refusal["link_id"] == "L-island-into-level"
    assert "no_such_function_node" in refusal["reason"], refusal
    wait_until_the_island_reading_probe_reads(1.5)

    stand_in_parent.send(
        {"cmd": "unfeed_island", "port": "level", "link_id": "L-island-into-level"}
    )
    wait_until_the_island_reading_probe_reads(None)

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "an island's answer rides its link's tag, and `unfeed_island` answers nothing"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_an_unlinked_input_reads_the_value_its_setup_carried_and_each_one_set_after(
    stand_in_parent,
):
    """The engine hands a helper the value each unlinked input reads on its
    setup command and again, whole, as an unanswered `set_input_values` on
    each change; a bool reads as 1 or 0, a value that is no number reaches no
    port, and an island fed into the port reads over its value until it is
    unfed.

    Fail-without-fix: drop the `set_input_values` arm from `_dispatch` and the
    probe keeps reading the setup's 0.25.
    """
    from helper_process_probes import LEVELS_THE_ISLAND_READING_PROBE_READ

    LEVELS_THE_ISLAND_READING_PROBE_READ.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:IslandReadingProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {"input_values": {"level": 0.25}},
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    wait_until_the_island_reading_probe_reads(0.25)

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": 0.5}})
    wait_until_the_island_reading_probe_reads(0.5)

    stand_in_parent.send(
        {"cmd": "feed_island", "island": an_island_feeding_the_probes_level(then_double_it=False)}
    )
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": "L-island-into-level"}
    wait_until_the_island_reading_probe_reads(0.75)
    stand_in_parent.send(
        {"cmd": "unfeed_island", "port": "level", "link_id": "L-island-into-level"}
    )
    wait_until_the_island_reading_probe_reads(0.5)

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": True}})
    wait_until_the_island_reading_probe_reads(1.0)

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": "loud"}})
    wait_until_the_island_reading_probe_reads(None)

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", "`set_input_values` answers nothing"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def wait_until_the_exposed_gain_probe_took(gain: float, timeout_s: float = 10.0) -> None:
    import helper_process_probes

    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if helper_process_probes.GAINS_THE_EXPOSED_GAIN_PROBE_TOOK[-1:] == [gain]:
            return
        time.sleep(0.01)
    raise AssertionError(
        f"the probe never took a gain of {gain!r}; the gains it took: "
        f"{helper_process_probes.GAINS_THE_EXPOSED_GAIN_PROBE_TOOK[-5:]}"
    )


def test_each_new_value_a_link_gives_an_exposed_config_field_is_a_live_config_update(
    stand_in_parent,
):
    """A config field the node exposes is read from the input port of its
    name before each `process()`, and each new value reaches the node as
    a live config update; a field it stops exposing is no longer read.

    Fail-without-fix: drop the read from `call_hook_from_the_scheduling_loop`
    and the probe keeps the setup's gain of 1.
    """
    from helper_process_probes import GAINS_THE_EXPOSED_GAIN_PROBE_TOOK

    GAINS_THE_EXPOSED_GAIN_PROBE_TOOK.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:ExposedGainProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {"gain": 1.0},
            "ports": {"exposed_config_fields": ["gain"]},
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    wait_until_the_exposed_gain_probe_took(1.0)

    island_into_the_gain = an_island_feeding_the_probes_level(then_double_it=True)
    island_into_the_gain["input_port"] = "gain"
    stand_in_parent.send({"cmd": "feed_island", "island": island_into_the_gain})
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": "L-island-into-level"}
    wait_until_the_exposed_gain_probe_took(1.5)
    configures_while_the_gain_held = len(GAINS_THE_EXPOSED_GAIN_PROBE_TOOK)
    time.sleep(0.1)
    assert len(GAINS_THE_EXPOSED_GAIN_PROBE_TOOK) == configures_while_the_gain_held, (
        "a value that did not change is no config update"
    )

    stand_in_parent.send({"cmd": "set_exposed_config_fields", "exposed_config_fields": []})
    stand_in_parent.send({"cmd": "update_config", "config": {"gain": 3.0}})
    assert stand_in_parent.receive() == {"rpc": "ok"}
    wait_until_the_exposed_gain_probe_took(3.0)
    time.sleep(0.1)
    assert GAINS_THE_EXPOSED_GAIN_PROBE_TOOK[-1] == 3.0, (
        "a field the node no longer exposes is not read from its port"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", "`set_exposed_config_fields` answers nothing"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_an_exposed_config_field_its_link_stops_driving_goes_back_to_its_config_value(
    stand_in_parent,
):
    """Unlinking an exposed field hands the node its config's own value
    again, the one the last config update set, as a live config update.

    Fail-without-fix: the probe keeps the 1.5 the island last gave its gain.
    """
    from helper_process_probes import GAINS_THE_EXPOSED_GAIN_PROBE_TOOK

    GAINS_THE_EXPOSED_GAIN_PROBE_TOOK.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:ExposedGainProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {"gain": 1.0},
            "ports": {"exposed_config_fields": ["gain"]},
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    stand_in_parent.send({"cmd": "update_config", "config": {"gain": 2.0}})
    assert stand_in_parent.receive() == {"rpc": "ok"}
    wait_until_the_exposed_gain_probe_took(2.0)

    island_into_the_gain = an_island_feeding_the_probes_level(then_double_it=True)
    island_into_the_gain["input_port"] = "gain"
    stand_in_parent.send({"cmd": "feed_island", "island": island_into_the_gain})
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": "L-island-into-level"}
    wait_until_the_exposed_gain_probe_took(1.5)

    stand_in_parent.send(
        {"cmd": "unfeed_island", "port": "gain", "link_id": "L-island-into-level"}
    )
    wait_until_the_exposed_gain_probe_took(2.0)

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", "`unfeed_island` answers nothing"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_wire_the_helper_cannot_open_is_answered_with_the_reason(stand_in_parent):
    """The engine reports a link `wired` only on this answer, so a port that
    could not open has to say so rather than only logging: the reason is what
    `graph` renders against the link, and the caller of a live `connect` reads
    it there.

    The refusal here is a port whose ring is deeper than the channel it is
    joining, which iceoryx2 refuses at `create_subscriber` — the shape a live
    connect onto a shallower channel takes.

    Fail-without-fix: log the failure and answer nothing, and the parent waits
    on an answer that never comes while `graph` reports the link pending
    forever.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    unopenable = engine_shaped_link_wiring("input", "L-unopenable")
    unopenable["channel_service_creation_depth"] = 4
    unopenable["input_port_ring_depth"] = 64
    stand_in_parent.send({"cmd": "wire_link", "direction": "input", "link": unopenable})

    answer = stand_in_parent.receive()
    assert answer["rpc"] == "link_wire_failed", (
        f"a port that could not open must be refused rather than reported open; got {answer}"
    )
    assert answer["link_id"] == "L-unopenable"
    assert "frames_from_upstream" in answer["reason"], (
        "`graph` renders this reason, so it has to name what could not open; "
        f"got {answer['reason']!r}"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "a refusal is link-scoped too, so it never becomes the reply to the "
        "next lifecycle command"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_wire_after_a_failed_setup_is_refused_rather_than_opened(stand_in_parent):
    """The engine can hand a link over while `setup` is still running, and the
    child reads it only once `setup` has answered. A node whose setup
    failed opens no port for it, so the link must read error rather than wired.

    Fail-without-fix: open the port anyway and the child answers `link_wired`
    for a node that never came up.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:RefusesSetupProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "output",
            "link": engine_shaped_link_wiring("output", "L-during-a-failed-setup"),
        }
    )
    assert stand_in_parent.receive()["rpc"] == "error"

    answer = stand_in_parent.receive()
    assert answer["rpc"] == "link_wire_failed", answer
    assert answer["link_id"] == "L-during-a-failed-setup"
    assert "setup did not succeed" in answer["reason"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_wire_in_an_unknown_direction_is_refused_rather_than_left_unanswered(
    stand_in_parent,
):
    """A direction the helper cannot act on is still a link the parent is
    waiting on. Answered as a refusal, the link reaches `error` in `graph`;
    left unanswered it would sit pending for the life of the runtime.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "sideways",
            "link": engine_shaped_link_wiring("input", "L-sideways"),
        }
    )

    answer = stand_in_parent.receive()
    assert answer["rpc"] == "link_wire_failed"
    assert answer["link_id"] == "L-sideways"

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_helper_slower_than_its_upstream_forwards_the_burst(stand_in_parent):
    """A node slower than its upstream never goes idle, so the loop runs
    `process()` back to back and still forwards what the burst sent."""

    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:SlowPassThroughProbe")
    )

    inbound_link_id = "L-slow-in"
    outbound_link_id = "L-slow-out"
    downstream = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        downstream,
        {"inputs": [engine_shaped_link_wiring("input", outbound_link_id)]},
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {
                "inputs": [engine_shaped_link_wiring("input", inbound_link_id)],
                "outputs": [engine_shaped_link_wiring("output", outbound_link_id)],
            },
        }
    )
    assert stand_in_parent.receive()["rpc"] == "ready"
    # Swapped in after setup, which handed the real object to the engine's
    # context; from here on only the loop reads it.
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    upstream = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        upstream,
        {"outputs": [engine_shaped_link_wiring("output", inbound_link_id)]},
    )
    # Faster than the probe processes, so its input never runs dry during the
    # burst and the loop never reaches its idle branch.
    upstream_pin = the_pin_upstream_of_a_pass_through_probe(upstream)
    for index in range(80):
        upstream_pin.write(frame_index_payload(index))
        time.sleep(0.001)

    forwarded = 0
    quiet_since = time.monotonic()
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and time.monotonic() - quiet_since < 0.3:
        if downstream.any_input_port_has_data():
            if downstream.read_from_input_port("frames_from_upstream") is not None:
                forwarded += 1
                quiet_since = time.monotonic()
            continue
        time.sleep(0.01)
    assert forwarded >= 8, f"the probe forwarded only {forwarded} messages of the burst"

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
