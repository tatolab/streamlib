# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Telemetry logging for Python subprocess processors.

Python subprocesses log to stderr. The Rust runtime captures stderr and
routes telemetry to the broker via gRPC (broker-as-collector pattern).
Uses only Python stdlib — no external deps.
"""

import logging


def setup_subprocess_telemetry(processor_id: str) -> logging.Logger:
    """Configure telemetry logging for a Python subprocess processor.

    Returns a logger that writes to stderr. The Rust host process captures
    stderr output and forwards it through the telemetry pipeline to the broker.
    """
    logger = logging.getLogger(f"streamlib.{processor_id}")
    logger.setLevel(logging.DEBUG)

    # Stderr handler — the primary output channel for subprocess logs
    stderr_handler = logging.StreamHandler()
    stderr_handler.setLevel(logging.DEBUG)
    stderr_handler.setFormatter(
        logging.Formatter(f"[streamlib:{processor_id}] %(message)s")
    )
    logger.addHandler(stderr_handler)

    return logger
