# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What the MoQ gateway's live fixtures share: the relay credential, the
scrubbing that keeps it out of every saved line, free ports, the independent
test subscriber's build, and the cannot-run exit.

CREDENTIALS. A draft-16 relay carries its token in the URL path, so the URL
is itself a credential. It is read from the environment only —
`STREAMLIB_MOQ_RELAY_URL`, or `CLOUDFLARE_MOQ_DRAFT_16_URL` plus
`CLOUDFLARE_MOQ_PUB_SUB_TOKEN` (`set -a; . ./.env; set +a` puts the repo's
there) — handed to child processes by environment, never argv, and scrubbed
from everything printed or saved. Absent credentials are a cannot-run.
"""

import os
import signal
import socket
import subprocess
import sys
from typing import NoReturn
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent
TEST_SUBSCRIBER_CRATE = FIXTURES / "moq_gateway_test_subscriber"
TEST_SUBSCRIBER = TEST_SUBSCRIBER_CRATE / "target" / "release" / "moq-gateway-test-subscriber"
LOCAL_RELAY_URL = "https://localhost:4443/local"

EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_CANNOT_RUN = 77


def cannot_run(why: str) -> NoReturn:
    """Stop with the cannot-run status: this machine cannot run the fixture."""
    print(f"CANNOT RUN: {why}", file=sys.stderr)
    raise SystemExit(EXIT_CANNOT_RUN)


def the_remote_relay_url() -> "str | None":
    """The remote relay's URL, token in the path, from the environment."""
    explicit = os.environ.get("STREAMLIB_MOQ_RELAY_URL", "").strip()
    if explicit:
        return explicit
    host = os.environ.get("CLOUDFLARE_MOQ_DRAFT_16_URL", "").strip()
    token = os.environ.get("CLOUDFLARE_MOQ_PUB_SUB_TOKEN", "").strip()
    if not host or not token:
        return None
    return f"https://{host.removeprefix('https://').strip('/')}/{token}"


def the_remote_relay_url_or_cannot_run() -> str:
    """The remote relay's URL, or a cannot-run naming what to set."""
    relay_url = the_remote_relay_url()
    if relay_url is None:
        cannot_run(
            "no relay credential. Export STREAMLIB_MOQ_RELAY_URL, or "
            "CLOUDFLARE_MOQ_DRAFT_16_URL and CLOUDFLARE_MOQ_PUB_SUB_TOKEN "
            "(`set -a; . ./.env; set +a`). Absent credentials are a cannot-run, never a pass."
        )
    return relay_url


def the_secrets_to_scrub() -> "list[str]":
    """Every credential string a saved line must never carry."""
    candidates = [
        os.environ.get("STREAMLIB_MOQ_RELAY_URL", ""),
        os.environ.get("CLOUDFLARE_MOQ_PUB_SUB_TOKEN", ""),
        os.environ.get("CLOUDFLARE_MOQ_SUB_TOKEN", ""),
        the_remote_relay_url() or "",
    ]
    for relay_url in list(candidates):
        if relay_url.startswith("https://"):
            candidates.append(relay_url.rsplit("/", 1)[-1])
    return sorted({secret.strip() for secret in candidates if len(secret.strip()) >= 8},
                  key=len, reverse=True)


def scrubbed(text: str) -> str:
    """`text` with every credential replaced by `<credential>`."""
    for secret in the_secrets_to_scrub():
        text = text.replace(secret, "<credential>")
    return text


def carries_a_credential(text: str) -> bool:
    """Whether `text` holds any credential verbatim."""
    return any(secret in text for secret in the_secrets_to_scrub())


def free_loopback_ports(count: int) -> "list[int]":
    """`count` distinct free loopback TCP ports, all held until all are known."""
    held = [socket.socket() for _ in range(count)]
    try:
        for one in held:
            one.bind(("127.0.0.1", 0))
        return [one.getsockname()[1] for one in held]
    finally:
        for one in held:
            one.close()


def the_engine_wheel_is_importable() -> None:
    """Cannot-run unless this interpreter imports the engine wheel."""
    probe = subprocess.run(
        [sys.executable, "-c", "import streamlib; streamlib.Runtime"],
        capture_output=True, text=True,
    )
    if probe.returncode != 0:
        last_line = (probe.stderr.strip().splitlines() or ["no error text"])[-1]
        cannot_run(
            f"{sys.executable} cannot import streamlib ({last_line}): build the wheel into "
            "this venv with `maturin develop` in sdk/streamlib-python-wheel"
        )


def build_the_test_subscriber(log_path: Path) -> Path:
    """Build the independent test subscriber, or cannot-run."""
    with open(log_path, "w") as build_log:
        built = subprocess.run(
            ["cargo", "build", "--release", "--locked", "--manifest-path",
             str(TEST_SUBSCRIBER_CRATE / "Cargo.toml")],
            stdout=build_log, stderr=subprocess.STDOUT,
        )
    if built.returncode != 0 or not TEST_SUBSCRIBER.exists():
        cannot_run(f"the test subscriber did not build; see {log_path}")
    return TEST_SUBSCRIBER


def start_the_local_relay(log_directory: Path) -> subprocess.Popen:
    """Build and start the pinned local draft-16 relay, or cannot-run."""
    relay_script = FIXTURES / "start_local_moq_relay.sh"
    with open(log_directory / "local-relay-build.log", "w") as build_log:
        built = subprocess.run(["bash", str(relay_script), "--build"],
                               stdout=build_log, stderr=subprocess.STDOUT)
    if built.returncode != 0:
        cannot_run(f"the local relay did not build; see {log_directory / 'local-relay-build.log'}")
    relay_log = open(log_directory / "local-relay.log", "w")
    relay = subprocess.Popen(["bash", str(relay_script)], stdout=relay_log,
                             stderr=subprocess.STDOUT, start_new_session=True)
    try:
        relay.wait(timeout=2)
    except subprocess.TimeoutExpired:
        return relay
    cannot_run(f"the local relay exited at once; see {log_directory / 'local-relay.log'}")


def stop_a_process_group(process: subprocess.Popen, grace_seconds: float = 20) -> None:
    """SIGTERM `process`'s group, then SIGKILL it past `grace_seconds`."""
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=grace_seconds)
    except ProcessLookupError:
        return
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)
