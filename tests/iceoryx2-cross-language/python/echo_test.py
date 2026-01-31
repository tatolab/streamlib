# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cross-language iceoryx2 validation test (Python side).

Receives a FramePayload from the Rust side, validates fields,
then echoes it back. Validates cross-language struct layout compatibility.

Usage:
    # In terminal 1 (start Rust publisher first):
    cargo run -p iceoryx2-cross-language-test

    # In terminal 2:
    python tests/iceoryx2-cross-language/python/echo_test.py

Requirements:
    pip install iceoryx2 msgpack
"""

import ctypes
import sys
import time

import iceoryx2
from iceoryx2 import ServiceName, ServiceType

# Add parent path to import streamlib frame_payload
sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parents[3] / "libs" / "streamlib-python" / "python"))

from streamlib.frame_payload import FramePayload, PortKey, SchemaName


def main():
    print("=== iceoryx2 Cross-Language Validation Test (Python) ===")
    print()

    # Print struct sizes for comparison with Rust
    print("Struct sizes:")
    print(f"  PortKey:      {ctypes.sizeof(PortKey)} bytes")
    print(f"  SchemaName:   {ctypes.sizeof(SchemaName)} bytes")
    print(f"  FramePayload: {ctypes.sizeof(FramePayload)} bytes")
    print()

    # Create iceoryx2 node
    node = iceoryx2.NodeBuilder.new().create(ServiceType.Ipc)

    # Subscribe to Rust publisher
    subscribe_service = (
        node.service_builder(ServiceName.new("streamlib/test-cross-lang-rust-to-python"))
        .publish_subscribe(FramePayload)
        .open_or_create()
    )
    subscriber = subscribe_service.subscriber_builder().create()

    # Publish echo back to Rust
    publish_service = (
        node.service_builder(ServiceName.new("streamlib/test-cross-lang-python-to-rust"))
        .publish_subscribe(FramePayload)
        .open_or_create()
    )
    publisher = publish_service.publisher_builder().create()

    print("Waiting for payload from Rust...")
    print()

    # Wait for payload
    timeout_s = 60
    poll_interval_s = 0.1
    start = time.monotonic()

    while True:
        sample = subscriber.receive()
        if sample is not None:
            # payload() returns a ctypes pointer - dereference with .contents
            payload_ptr = sample.payload()
            payload = payload_ptr.contents

            port = payload.get_port()
            schema = payload.get_schema()
            timestamp = payload.timestamp_ns
            data = payload.get_data()

            print("Received payload from Rust:")
            print(f"  port_key:     '{port}'")
            print(f"  schema_name:  '{schema}'")
            print(f"  timestamp_ns: {timestamp}")
            print(f"  data_len:     {len(data)} bytes")
            print()

            # Validate
            passed = True

            if port != "test_port":
                print(f"FAIL: port_key mismatch: expected 'test_port', got '{port}'")
                passed = False
            if schema != "test_schema":
                print(f"FAIL: schema_name mismatch: expected 'test_schema', got '{schema}'")
                passed = False
            if timestamp != 12345:
                print(f"FAIL: timestamp_ns mismatch: expected 12345, got {timestamp}")
                passed = False

            # Verify msgpack data
            try:
                import msgpack

                decoded = msgpack.unpackb(data, raw=False)
                if decoded.get("hello") != "world":
                    print(f"FAIL: data['hello'] mismatch: expected 'world', got '{decoded.get('hello')}'")
                    passed = False
                if decoded.get("count") != 42:
                    print(f"FAIL: data['count'] mismatch: expected 42, got {decoded.get('count')}")
                    passed = False
                print(f"  Decoded data: {decoded}")
            except Exception as e:
                print(f"FAIL: Failed to decode msgpack data: {e}")
                passed = False

            print()

            # Echo the payload back to Rust
            print("Echoing payload back to Rust...")
            echo_sample = publisher.loan_uninit()
            if echo_sample is not None:
                echo_payload = FramePayload()
                echo_payload.set_data(port, schema, timestamp, data)
                initialized = echo_sample.write_payload(echo_payload)
                initialized.send()
                print("Echo sent!")
            else:
                print("FAIL: Could not loan sample for echo")
                passed = False

            print()

            if passed:
                print("=== ALL PYTHON TESTS PASSED ===")
            else:
                print("=== SOME PYTHON TESTS FAILED ===")
                sys.exit(1)

            break

        elapsed = time.monotonic() - start
        if elapsed > timeout_s:
            print(f"TIMEOUT: No payload received from Rust within {timeout_s}s")
            sys.exit(1)

        time.sleep(poll_interval_s)


if __name__ == "__main__":
    main()
