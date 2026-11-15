# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The environment variables a processor interpreter's parent starts it with."""

ENTRYPOINT_ENV = "STREAMLIB_ENTRYPOINT"
PROCESSOR_ID_ENV = "STREAMLIB_PROCESSOR_ID"
RUNTIME_ID_ENV = "STREAMLIB_RUNTIME_ID"
ESCALATE_FD_ENV = "STREAMLIB_ESCALATE_FD"
# The socket the helper's native code escalates over, read by the engine
# module itself; the bootstrap only keeps it out of what the processor starts.
NATIVE_ESCALATE_FD_ENV = "STREAMLIB_NATIVE_ESCALATE_FD"
ENGINE_BUILD_ID_ENV = "STREAMLIB_ENGINE_BUILD_ID"
