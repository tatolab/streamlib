# CLAUDE.md

## Licensing

Please include the following header in every new Rust and Python file outside `vendor/`, with
`#` in place of `//` in Python. `LICENSE` holds the terms.

    // Copyright (c) 2025 Tatolab LLC
    // SPDX-License-Identifier: GPL-3.0-only

---

StreamLib is a GPL-3.0-licensed real-time streaming processing runtime like Nvidia holoscan.It is built like a game engine:
ONE core system per concern — extend the existing system, never build a parallel one. Search first.

A node is one unit in a stream's graph. "Processor" is not a word in this tree: say node
(`scripts/check_processor_word.py` fails on it; `lint-exceptions/processor-word.txt` only shrinks).

## How work usually flows

Skills describe how the owner likes work reasoned about; adapt or skip them as needed.
Skills, agents, the repository rules and hooks come from `dev-plugin` in
[tatolab/tato-plugins-official](https://github.com/tatolab/tato-plugins-official), which
`.claude/settings.json` enables; change them there, never in this repository.

- **Work is tracked in Linear, never in GitHub issues** (settings deny `gh issue` writes and the GitHub MCP issue tools):
  team Tatolab, issues `TAT-n`, structured project → milestone → issue → sub-issue. An issue
  that lands here is titled `streamlib: …`; a change spanning repos is a parent with one
  sub-issue per repo.
- Never write documentation, plans or ADRs, and never create Linear projects or milestones; planning happens in Linear, in separate sessions.
- Code is the only source of truth.
