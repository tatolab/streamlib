# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Engine↔SDK subprocess protocol-version handshake.

The single coordinate the engine and this SDK agree on so a `streamlib`
resolved from a registry *by version* refuses to run against an incompatible
engine — instead of failing deep in the FFI / escalate / lifecycle path with a
cryptic crash. Mirrors the engine-side `STREAMLIB_SUBPROCESS_PROTOCOL_VERSION`;
covers the three lockstep runtime surfaces (native-lib ctypes FFI, escalate
IPC, stdin/stdout lifecycle-command protocol). This is the Python-subprocess
analogue of the cdylib plugin ABI's `STREAMLIB_ABI_VERSION`.

The contract is a **monotonic range, not strict equality** (the Cloudflare
`compatibility_date` shape): this SDK can speak any engine protocol in
``[MIN_ENGINE_PROTOCOL, PROTOCOL_VERSION]``, so a newer SDK keeps working
against a range of older engines. Bump `PROTOCOL_VERSION` (in lockstep with the
engine constant) when any of the three surfaces changes incompatibly; raise
`MIN_ENGINE_PROTOCOL` only when dropping support for an old engine protocol.
"""

import os

#: The subprocess protocol version this SDK implements.
PROTOCOL_VERSION = 1

#: Oldest engine protocol version this SDK can still speak.
MIN_ENGINE_PROTOCOL = 1

#: Env var the engine sets to advertise its protocol version to the subprocess.
ENGINE_PROTOCOL_ENV = "STREAMLIB_PROTOCOL_VERSION"


class ProtocolMismatchError(RuntimeError):
    """The engine's subprocess protocol version is one this SDK can't speak."""


def engine_protocol_from_env() -> int:
    """Read the engine's advertised protocol version from the environment.

    Raises [`ProtocolMismatchError`] when unset (the host doesn't speak the
    streamlib subprocess protocol — an engine older than this SDK, or a process
    not launched by streamlib) or non-integer."""
    raw = os.environ.get(ENGINE_PROTOCOL_ENV)
    if not raw:
        raise ProtocolMismatchError(
            f"{ENGINE_PROTOCOL_ENV} not set — the host does not speak the "
            f"streamlib subprocess protocol (this SDK implements "
            f"v{PROTOCOL_VERSION}). The engine is older than this streamlib, or "
            f"this process was not launched by a streamlib runtime."
        )
    try:
        return int(raw)
    except ValueError:
        raise ProtocolMismatchError(
            f"{ENGINE_PROTOCOL_ENV}={raw!r} is not an integer protocol version"
        ) from None


def assert_engine_compatible(engine_version: int) -> None:
    """Fail loud when the engine's protocol version is outside this SDK's range."""
    if not (MIN_ENGINE_PROTOCOL <= engine_version <= PROTOCOL_VERSION):
        raise ProtocolMismatchError(
            f"engine speaks subprocess protocol v{engine_version}, this "
            f"streamlib SDK speaks v{MIN_ENGINE_PROTOCOL}..v{PROTOCOL_VERSION}. "
            f"The installed streamlib is incompatible with this engine — align "
            f"the package's declared streamlib version to the engine's."
        )
