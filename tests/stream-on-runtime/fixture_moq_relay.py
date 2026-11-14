# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The `moq-relay` a test's gateway nodes dial, started on loopback for that test alone.

The relay is the version `packages/tatolab-moq` tests against, built by
`cargo install` into `<repository>/target/fixture-relay` — the install root the
engine's fixture drivers use (`fixture_runtime_unit.sh`) — the first time a
test needs it. It is open to anonymous publish and subscribe, and serves its
QUIC and its certificate's fingerprint on one port.
"""

from __future__ import annotations

import subprocess
import time
import urllib.request
from collections.abc import Iterator
from pathlib import Path

import pytest

from conftest import a_tcp_port_free_on_loopback
from runtime_unit_under_test import REPOSITORY_ROOT

FIXTURE_RELAY_CRATE_VERSION = "0.17.7"
FIXTURE_RELAY_INSTALL_ROOT = REPOSITORY_ROOT / "target" / "fixture-relay"
FIXTURE_RELAY_EXECUTABLE = FIXTURE_RELAY_INSTALL_ROOT / "bin" / "moq-relay"

#: How long the relay has to serve its fingerprint once started.
FIXTURE_RELAY_SERVING_BUDGET_SECONDS = 30.0


def the_fixture_relay_executable() -> Path:
    """The relay's executable, built into its install root when it is absent."""
    if not FIXTURE_RELAY_EXECUTABLE.exists():
        subprocess.run(
            [
                "cargo",
                "install",
                "moq-relay",
                "--version",
                f"={FIXTURE_RELAY_CRATE_VERSION}",
                "--locked",
                "--no-default-features",
                "--features",
                "noq",
                "--root",
                str(FIXTURE_RELAY_INSTALL_ROOT),
            ],
            check=True,
        )
    return FIXTURE_RELAY_EXECUTABLE


def fixture_relay_configuration(port: int) -> str:
    """The relay's TOML: loopback only, a generated certificate, anonymous access."""
    return f"""drain_timeout = "0s"

[listen]
bind = "127.0.0.1:{port}"

[listen.tls]
generate = ["localhost"]

[web.http]
listen = "127.0.0.1:{port}"

# The relay's own dial socket, on IPv4 too, for a host with no IPv6.
[connect]
bind = "127.0.0.1:0"

[auth]
public = ["**"]
"""


def the_relay_serves_its_fingerprint(relay_url: str) -> bool:
    try:
        with urllib.request.urlopen(f"{relay_url}/certificate.sha256", timeout=1) as answer:
            return bool(answer.read().strip())
    except OSError:
        return False


@pytest.fixture
def fixture_moq_relay_url(tmp_path: Path) -> "Iterator[str]":
    """Start a relay for this test and yield its `http://127.0.0.1:<port>` URL."""
    port = a_tcp_port_free_on_loopback()
    configuration_path = tmp_path / "moq-relay.toml"
    configuration_path.write_text(fixture_relay_configuration(port))
    relay_log_path = tmp_path / "moq-relay.log"
    with relay_log_path.open("w") as relay_log:
        relay = subprocess.Popen(
            [str(the_fixture_relay_executable()), str(configuration_path)],
            stdin=subprocess.DEVNULL,
            stdout=relay_log,
            stderr=subprocess.STDOUT,
        )
    relay_url = f"http://127.0.0.1:{port}"
    try:
        serving_deadline = time.monotonic() + FIXTURE_RELAY_SERVING_BUDGET_SECONDS
        while not the_relay_serves_its_fingerprint(relay_url):
            if relay.poll() is not None or time.monotonic() > serving_deadline:
                pytest.fail(
                    f"the fixture relay did not serve on port {port}:\n{relay_log_path.read_text()}"
                )
            time.sleep(0.1)
        yield relay_url
    finally:
        relay.terminate()
        try:
            relay.wait(timeout=5)
        except subprocess.TimeoutExpired:
            relay.kill()
            relay.wait()
