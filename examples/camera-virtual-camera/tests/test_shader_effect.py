# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""`ShaderEffect`, driven as a real app: each shipped look, and a broken one.

Every test here boots an engine and builds a kernel, so every test needs the
GPU: the shader compiler runs at kernel construction, behind the GPU context.
They run on a machine with one, from this example's venv — `uv run pytest`.
"""

import json
import math
import os
import queue
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path

import pytest
from shader_effect_test_processors import (
    KNOWN_PATTERN_HEIGHT,
    KNOWN_PATTERN_WIDTH,
    RENDERED_PIXELS_MARKER,
    known_pattern_pixel_at,
)

EXAMPLE_DIRECTORY = Path(__file__).resolve().parent.parent
TEST_APP = Path(__file__).resolve().parent / "shader_effect_test_app.py"

# A cold engine boot stands up a GPU context and a helper interpreter per
# processor; a real hang blows through it.
FIRST_RENDERED_FRAME_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0

# An 8-bit target rounds, and a sampler may land a hair off a texel centre.
PIXEL_CHANNEL_TOLERANCE = 2


class RunningTestApp:
    """A `python shader_effect_test_app.py …` with its output pumped off the pipe."""

    def __init__(self, *arguments: str) -> None:
        environment = dict(os.environ)
        environment["PYTHONPATH"] = os.pathsep.join(
            path
            for path in (str(EXAMPLE_DIRECTORY), environment.get("PYTHONPATH", ""))
            if path
        )
        self.process = subprocess.Popen(
            [sys.executable, str(TEST_APP), *arguments],
            cwd=TEST_APP.parent,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
        self.output_lines: list[str] = []
        self._output_lines_pumped_from_the_app_pipe: queue.Queue[str | None] = (
            queue.Queue()
        )
        threading.Thread(target=self._pump_output, daemon=True).start()

    def _pump_output(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self._output_lines_pumped_from_the_app_pipe.put(line)
        self._output_lines_pumped_from_the_app_pipe.put(None)

    @property
    def output(self) -> str:
        return "".join(self.output_lines)

    def await_line(
        self, matches: "Callable[[str], bool]", what: str, timeout: float
    ) -> str:
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError(f"no {what} within {timeout} s:\n{self.output}")
            try:
                line = self._output_lines_pumped_from_the_app_pipe.get(
                    timeout=min(0.5, remaining)
                )
            except queue.Empty:
                continue
            if line is None:
                raise AssertionError(f"the app exited before {what}:\n{self.output}")
            self.output_lines.append(line)
            if matches(line):
                return line

    def interrupt_and_await_clean_exit(self) -> None:
        self.process.send_signal(signal.SIGINT)
        self.await_line(
            lambda line: "MARKER:CLEAN_EXIT" in line,
            "clean exit",
            CLEAN_EXIT_TIMEOUT_SECONDS,
        )
        assert self.process.wait(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, self.output

    def kill_process_group(self) -> None:
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait()


@pytest.fixture
def start_test_app():
    """Hands out apps and kills their process groups however a test ends, so a
    failed assertion never strands an engine holding the GPU."""
    started: list[RunningTestApp] = []

    def start(*arguments: str) -> RunningTestApp:
        app = RunningTestApp(*arguments)
        started.append(app)
        return app

    try:
        yield start
    finally:
        for app in started:
            app.kill_process_group()


def rendered_pixels_report_in(line: str) -> dict:
    # Decoded up to the object's end: a forwarded helper record carries the
    # processor's id after the message.
    report, _ = json.JSONDecoder().raw_decode(line.split(RENDERED_PIXELS_MARKER, 1)[1])
    return report


def assert_rendered_matches_the_reference(
    rendered: "list[int]", expected: "list[int]", what: str
) -> None:
    assert all(
        abs(rendered_channel - expected_channel) <= PIXEL_CHANNEL_TOLERANCE
        for rendered_channel, expected_channel in zip(rendered, expected, strict=True)
    ), f"{what}: rendered {rendered}, the CPU reference is {expected}"


def unorm8(value: float) -> int:
    return round(min(max(value, 0.0), 1.0) * 255)


def grayscale_reference_at(x: int, y: int) -> "list[int]":
    red, green, blue, alpha = known_pattern_pixel_at(x, y)
    luma = (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255
    return [unorm8(luma)] * 3 + [alpha]


def vignette_reference_at(x: int, y: int) -> "list[int]":
    red, green, blue, alpha = known_pattern_pixel_at(x, y)
    screen_u = (x + 0.5) / KNOWN_PATTERN_WIDTH
    screen_v = (y + 0.5) / KNOWN_PATTERN_HEIGHT
    distance_from_centre = math.hypot(screen_u - 0.5, screen_v - 0.5)
    fade = min(max((distance_from_centre - 0.35) / (0.8 - 0.35), 0.0), 1.0)
    light_kept = 1.0 - fade * fade * (3.0 - 2.0 * fade)
    return [unorm8(channel / 255 * light_kept) for channel in (red, green, blue)] + [
        alpha
    ]


def pixelate_reference_at(x: int, y: int) -> "list[int]":
    cell_size = 16
    cell_centre_x = min(
        (x // cell_size) * cell_size + cell_size // 2, KNOWN_PATTERN_WIDTH - 1
    )
    cell_centre_y = min(
        (y // cell_size) * cell_size + cell_size // 2, KNOWN_PATTERN_HEIGHT - 1
    )
    return list(known_pattern_pixel_at(cell_centre_x, cell_centre_y))


# One pixel per look, each chosen where the look changes the pattern: a corner
# the vignette darkens without blacking out, and a pixelate cell whose centre
# is not the pixel itself.
SHIPPED_LOOKS = [
    pytest.param("grayscale.frag", (37, 21), grayscale_reference_at, id="grayscale"),
    pytest.param("vignette.frag", (5, 4), vignette_reference_at, id="vignette"),
    pytest.param("pixelate.frag", (20, 37), pixelate_reference_at, id="pixelate"),
]


@pytest.mark.parametrize(("shader_file_name", "pixel", "reference_at"), SHIPPED_LOOKS)
def test_a_shipped_look_renders_the_cpu_reference_over_a_buffer_backed_frame(
    start_test_app, shader_file_name, pixel, reference_at
):
    x, y = pixel
    expected = reference_at(x, y)
    assert expected != list(known_pattern_pixel_at(x, y)), (
        "the chosen pixel must be one the look changes, or a pass that forwarded "
        "the frame untouched would pass"
    )

    app = start_test_app("one_shipped_look", shader_file_name, json.dumps([[x, y]]))
    report = rendered_pixels_report_in(
        app.await_line(
            lambda line: RENDERED_PIXELS_MARKER in line,
            f"rendered frame from {shader_file_name}",
            FIRST_RENDERED_FRAME_TIMEOUT_SECONDS,
        )
    )
    app.interrupt_and_await_clean_exit()

    assert report["extent"] == [KNOWN_PATTERN_WIDTH, KNOWN_PATTERN_HEIGHT]
    assert_rendered_matches_the_reference(
        report["pixels"][0], expected, f"{shader_file_name} at {pixel}"
    )


def test_a_look_that_does_not_compile_is_refused_at_setup_and_the_graph_keeps_running(
    start_test_app,
):
    app = start_test_app("a_look_that_does_not_compile_beside_one_that_does")

    # The engine's record of the failed setup names the processor, then carries
    # the raised message and the compiler's diagnostic on the lines after it.
    app.await_line(
        lambda line: "Setup failed" in line and "[ShaderEffect 2]" in line,
        "the engine refusing the broken look by its display name",
        FIRST_RENDERED_FRAME_TIMEOUT_SECONDS,
    )
    refusal_message = app.await_line(
        lambda line: "ShaderEffect could not build its pass" in line,
        "the broken look's refusal message",
        FIRST_RENDERED_FRAME_TIMEOUT_SECONDS,
    )
    assert "fragment_glsl" in refusal_message, (
        f"the refusal must name the config key: {refusal_message}"
    )
    app.await_line(
        lambda line: "error" in line and "no_such_function" in line,
        "the compiler's own diagnostic in the refusal",
        FIRST_RENDERED_FRAME_TIMEOUT_SECONDS,
    )

    # Frames reported *after* the refusal: the working chain kept running
    # through it rather than having finished before it.
    first_report_after = rendered_pixels_report_in(
        app.await_line(
            lambda line: RENDERED_PIXELS_MARKER in line,
            "a rendered frame after the refusal",
            FIRST_RENDERED_FRAME_TIMEOUT_SECONDS,
        )
    )
    later_report = rendered_pixels_report_in(
        app.await_line(
            lambda line: (
                RENDERED_PIXELS_MARKER in line
                and rendered_pixels_report_in(line)["frame"]
                >= first_report_after["frame"] + 10
            ),
            "ten more rendered frames",
            FIRST_RENDERED_FRAME_TIMEOUT_SECONDS,
        )
    )
    app.interrupt_and_await_clean_exit()

    assert_rendered_matches_the_reference(
        later_report["pixels"][0],
        grayscale_reference_at(0, 0),
        "the working grayscale chain at (0, 0)",
    )
