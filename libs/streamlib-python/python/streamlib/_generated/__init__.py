# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Generated gRPC stubs for broker.proto.

These files are auto-generated and should not be edited manually.
To regenerate:
    cd libs/streamlib-python
    uv run python -m grpc_tools.protoc \
        -I ../streamlib-broker/proto \
        --python_out=python/streamlib/_generated \
        --grpc_python_out=python/streamlib/_generated \
        broker.proto
"""

from . import broker_pb2
from . import broker_pb2_grpc

__all__ = ["broker_pb2", "broker_pb2_grpc"]
