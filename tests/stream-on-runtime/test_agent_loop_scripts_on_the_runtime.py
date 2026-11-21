# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Each agent loop script, replayed over MCP against a runtime unit holding one running stream.

The project carries every node file a script's task names, and its stream runs one test
pattern source; the script learns everything else from the session.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from agent_loop_script_runner import (
    every_agent_loop_script_path,
    load_agent_loop_script,
    play_agent_loop_script,
)
from cli_launch_projects import NODE_READY_TIMEOUT_SECONDS
from conftest import TatolabdUnderTest
from stream_runs_on_tatolabd import TatolabRunOfAProject
from test_cli_launch import STREAM_WITH_ONE_TEST_PATTERN_SOURCE

pytestmark = pytest.mark.requires_gpu

AGENT_LOOP_SESSION_TIMEOUT_SECONDS = 180.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0

#: The function node `build_a_graph`'s task names as `nodes.clamp:Clamp`, whose number input a
#: picture output does not connect to.
CLAMP_SOURCE = '''\
from tatolab.stream import node


@node
def Clamp(x: float = 0.0, low: float = 0.0, high: float = 1.0) -> float:  # noqa: N802
    """`x` held between `low` and `high`."""
    return min(max(x, low), high)
'''

#: Where a project's installed extension SDK sits, the last place the runtime looks for a kit.
PROJECT_INSTALLED_EXTENSION_SDK = "node_modules/@tatolab/tato-app-view-sdk"

#: A stand-in extension SDK kit, as Tato Studio's SDK writes one, holding the control template
#: `author_a_node_ui_element`'s task starts from and the few declarations it names. The runtime
#: unit's own compiler and linter check against it.
STAND_IN_EXTENSION_SDK_KIT_FILES = {
    "package.json": """\
{
  "name": "@tatolab/tato-app-view-sdk",
  "version": "0.0.0-stand-in",
  "type": "module",
  "exports": {
    "./node_ui": { "types": "./node_ui.d.ts" },
    "./node_ui/input_control.css": "./input_control.css",
    "./tsconfig.preset.json": "./tsconfig.preset.json",
    "./package.json": "./package.json"
  }
}
""",
    "check_folder/package.json": '{ "private": true, "type": "module" }\n',
    "check_folder/tsconfig.json": """\
{
  "extends": "@tatolab/tato-app-view-sdk/tsconfig.preset.json",
  "include": ["**/*.ts"],
  "exclude": ["node_modules"]
}
""",
    "tsconfig.preset.json": """\
{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "noImplicitOverride": true,
    "skipLibCheck": true,
    "erasableSyntaxOnly": true,
    "verbatimModuleSyntax": true,
    "allowImportingTsExtensions": true,
    "module": "nodenext",
    "moduleResolution": "nodenext",
    "target": "ES2023",
    "lib": ["ES2023", "DOM", "DOM.Iterable"],
    "types": [],
    "noEmit": true
  },
  "files": ["./css_module_scripts.d.ts"]
}
""",
    "css_module_scripts.d.ts": """\
declare module "*.css" {
  const stylesheet: CSSStyleSheet;
  export default stylesheet;
}
""",
    "input_control.css": "",
    "node_ui.d.ts": """\
export type InputControlState = "editable" | "read_only" | "driven_by_wire";
export interface PinValues {
  readonly float: number;
  readonly bool: boolean;
  readonly string: string;
}
export type StoredPinValue<TypeName extends string> = TypeName extends keyof PinValues ? PinValues[TypeName] : null;
export interface InputPinDeclaration<TypeName extends string> {
  readonly input_name: string;
  readonly label: string;
  readonly pin_type: TypeName;
}
export interface ControlledPin<TypeName extends string> {
  readonly declaration: InputPinDeclaration<TypeName>;
}
export interface InputControlElement<TypeName extends string> extends HTMLElement {
  pin: ControlledPin<TypeName>;
  value: StoredPinValue<TypeName>;
  state: InputControlState;
}
export type InputControlElementClass<TypeName extends string> = new () => InputControlElement<TypeName>;
""",
    "tie_lines.json": """\
{
  "header": "import type { InputControlElementClass } from \\"@tatolab/tato-app-view-sdk/node_ui\\";",
  "controls": "import Control{index} from \\"./{script}\\";\\nexport const control{index}: InputControlElementClass<\\"{type}\\"> = Control{index};"
}
""",
    "oxlint.json": """\
{
  "plugins": ["typescript"],
  "categories": { "correctness": "off" },
  "rules": {
    "typescript/no-explicit-any": "error",
    "typescript/no-non-null-assertion": "error"
  }
}
""",
    "templates/control.ts": """\
import type { ControlledPin, InputControlElement, InputControlState, StoredPinValue } from "@tatolab/tato-app-view-sdk/node_ui";
import controlStyles from "@tatolab/tato-app-view-sdk/node_ui/input_control.css" with { type: "css" };

type PinTypeName = "PIN_TYPE_NAME";

export default class InputValueInWords extends HTMLElement implements InputControlElement<PinTypeName> {
  readonly #words = document.createElement("output");

  constructor() {
    super();
    const shadow = this.attachShadow({ mode: "open" });
    shadow.adoptedStyleSheets = [controlStyles];
    shadow.append(this.#words);
  }

  set pin({ declaration }: ControlledPin<PinTypeName>) {
    this.#words.setAttribute("aria-label", declaration.label);
  }

  set value(value: StoredPinValue<PinTypeName>) {
    this.#words.value = JSON.stringify(value);
  }

  set state(state: InputControlState) {
    this.#words.dataset["state"] = state;
  }
}
""",
}

#: The files of the project every script runs on.
AGENT_LOOP_PROJECT_FILES = {
    "nodes/__init__.py": "",
    "nodes/clamp.py": CLAMP_SOURCE,
    "stream.py": STREAM_WITH_ONE_TEST_PATTERN_SOURCE,
    **{
        f"{PROJECT_INSTALLED_EXTENSION_SDK}/{kit_file}": text
        for kit_file, text in STAND_IN_EXTENSION_SDK_KIT_FILES.items()
    },
}


@pytest.mark.parametrize(
    "script_path",
    every_agent_loop_script_path(),
    ids=lambda script_path: script_path.stem,
)
def test_an_agent_loop_script_plays_through_on_a_runtime_unit(
    script_path: Path,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
) -> None:
    script = load_agent_loop_script(script_path)
    project_directory = make_tatolab_project(AGENT_LOOP_PROJECT_FILES)
    tatolabd = start_tatolabd()
    tatolab_run = tatolabd.run_stream(
        TatolabRunOfAProject(working_directory=project_directory)
    )
    tatolab_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    local_api = tatolabd.local_api_client()

    answers: "dict[str, Any]" = local_api.answer_over_mcp(
        lambda client: asyncio.wait_for(
            play_agent_loop_script(client, script), AGENT_LOOP_SESSION_TIMEOUT_SECONDS
        ),
        timeout=AGENT_LOOP_SESSION_TIMEOUT_SECONDS,
    )

    assert list(answers) == [step.name for step in script.steps]
    assert tatolab_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0
