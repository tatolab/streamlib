# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The stand-in parent a helper process test speaks to, over the same framing a
helper's parent uses."""

import json
import select
import socket
import struct
import time


FRAME_LENGTH_PREFIX = struct.Struct(">I")

# The commands a helper answers, which a parent stamps with the id their answer
# echoes.
COMMANDS_ANSWERED_BY_A_REPLY = frozenset(
    {"setup", "stop", "teardown", "on_pause", "on_resume", "carry_links", "wire_link", "feed_island"}
)


class StandInParent:
    """The parent half of the socketpair, speaking the same framing.

    Reads go through `select` on the raw socket rather than a buffered file
    with a timeout: one timed-out read poisons a socket file object for good
    ("cannot read from timed out object"), and several tests here deliberately
    wait for a reply that must not come.
    """

    def __init__(self) -> None:
        self.parent_end, self.child_end = socket.socketpair()
        self._next_command_id = 1
        self._command_ids_stamped_here: "set[int]" = set()

    def send(self, message: dict) -> None:
        """Send `message`, stamped with a command id as the parent stamps every
        command it awaits an answer to, unless the test named one itself."""
        if message.get("cmd") in COMMANDS_ANSWERED_BY_A_REPLY and "command_id" not in message:
            message = {**message, "command_id": self._next_command_id}
            self._command_ids_stamped_here.add(self._next_command_id)
            self._next_command_id += 1
        payload = json.dumps(message).encode("utf-8")
        self.parent_end.sendall(FRAME_LENGTH_PREFIX.pack(len(payload)) + payload)

    def receive(self, timeout_seconds: float = 5.0):
        """The next frame, or `None` if none arrives inside the deadline. The id
        an answer echoes is taken off it when this stand-in stamped it, so a
        test reads the answer alone."""
        deadline = time.monotonic() + timeout_seconds
        length_prefix = self._read_exactly(FRAME_LENGTH_PREFIX.size, deadline)
        if length_prefix is None:
            return None
        (payload_length,) = FRAME_LENGTH_PREFIX.unpack(length_prefix)
        payload = self._read_exactly(payload_length, deadline)
        if payload is None:
            return None
        frame = json.loads(payload.decode("utf-8"))
        if frame.get("command_id") in self._command_ids_stamped_here:
            self._command_ids_stamped_here.discard(frame.pop("command_id"))
        return frame

    def receive_answering_each_escalate(
        self, timeout_seconds: float = 5.0, escalates_answered: "list | None" = None
    ):
        """The next frame that is neither a log nor an escalate request, or `None` if
        none arrives inside the deadline; each escalate request on the way is
        answered `ok`, and kept in `escalates_answered` when one is given."""
        deadline = time.monotonic() + timeout_seconds
        while True:
            frame = self.receive(max(deadline - time.monotonic(), 0.0))
            if frame is None:
                return None
            if frame.get("rpc") == "escalate_request":
                if escalates_answered is not None:
                    escalates_answered.append(frame)
                self.send(
                    {"rpc": "escalate_response", "request_id": frame["request_id"], "result": "ok"}
                )
            elif frame.get("op") != "log":
                return frame

    def _read_exactly(self, byte_count: int, deadline: float):
        collected = bytearray()
        while len(collected) < byte_count:
            remaining_seconds = deadline - time.monotonic()
            if remaining_seconds <= 0:
                return None
            readable, _, _ = select.select([self.parent_end], [], [], remaining_seconds)
            if not readable:
                return None
            chunk = self.parent_end.recv(byte_count - len(collected))
            if not chunk:
                return None
            collected.extend(chunk)
        return bytes(collected)

    def close(self) -> None:
        self.parent_end.close()
